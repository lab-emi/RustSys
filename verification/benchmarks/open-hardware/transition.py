# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Inductive state-interface refinement, checked by external ABC CEC.

Every storage bit is either related to implementation state or is independently
universally quantified. The initial relation must cover all implementation
initial states. Equal outputs, next-state data, clock events and asynchronous
reset controls then preserve that relation for every event; no reset sequence
or bounded-unrolling assumption is introduced. Unsupported storage fails closed.

This module reads only standard output models from external tools. It does not
implement a Boolean solver or inspect reference-tool implementation sources.
"""
from dataclasses import dataclass
from pathlib import Path
import json
import re
import collections
import hashlib
import time
import subprocess
import sys
import gc

import run
import equiv


@dataclass(frozen=True)
class Limits:
    max_cells: int = 1_100_000
    max_bits: int = 2_000_000
    max_state: int = 200_000
    max_connections: int = 16_000_000
    max_json_bytes: int = 1024**3
    max_batch_outputs: int = 32
    whole_attempt_s: int = 600
    cec_engine: str = 'gia'
    constant_exclusions: tuple[str, ...] = ()

    def __post_init__(self):
        if any(type(value) is not int or value <= 0 for name, value in self.__dict__.items() if name not in ("whole_attempt_s", "cec_engine", "constant_exclusions")):
            raise ValueError("transition budgets must be positive integers")
        if self.cec_engine not in ('legacy', 'gia'):
            raise ValueError('unknown ABC combinational equivalence engine')
        if type(self.whole_attempt_s) is not int or self.whole_attempt_s < 0:
            raise ValueError("whole-design attempt budget must be a nonnegative integer")
        if self.max_json_bytes > 1024**3:
            raise ValueError("transition JSON file budget cannot exceed 1 GiB")
        if self.max_batch_outputs > 64:
            raise ValueError("at most 64 outputs per explicit proof batch")
        if len(self.constant_exclusions) > 128 or any(not isinstance(name,str) or not name for name in self.constant_exclusions):
            raise ValueError('constant relation exclusion budget reached')


SEQ = {"$dff", "$adff", "$dlatch", "$_DFF_P_", "$_DFF_N_"}
SEQ.update(f"$_DFF_{c}{r}{v}_" for c in "PN" for r in "PN" for v in "01")
SEQ.update(f"$_DLATCH_{p}_" for p in "PN")

def intval(p, n, default=0):
    s = p.get(n, default)
    return int(s, 2) if isinstance(s, str) else int(s)

def normbit(x):
    return 'x' if x == 'z' else x

def next_bit_id(model):
    """Include unconnected ports/names when reserving a fresh signal ID."""
    maximum = 1
    for cell in model['cells'].values():
        for bits in cell['connections'].values():
            for bit in bits:
                if isinstance(bit, int):
                    maximum = max(maximum, bit)
    for table in ('ports', 'netnames'):
        for entry in model.get(table, {}).values():
            for bit in entry['bits']:
                if isinstance(bit, int):
                    maximum = max(maximum, bit)
    return maximum + 1

def anonymous_scope_pairs(aliases, states, limit):
    """Propose only unambiguous aliases across omitted anonymous scopes.

    Different frontends may retain or omit ``genblkN`` scope components.
    This is a state-relation proposal, never an assumption: all proposed
    state data, controls and initial values still have to be proved below.
    Array indices and explicitly named scopes remain significant.
    """
    folded = []
    for names, state in zip(aliases, states):
        by_name = {}
        count = 0
        for name, bit in names.items():
            if bit not in state:
                continue
            key = '.'.join(part for part in name.split('.') if not re.fullmatch(r'genblk[0-9]+', part))
            by_name.setdefault(key, set()).add(bit)
            count += 1
            if count > limit:
                raise ValueError('anonymous-scope alias budget reached')
        folded.append(by_name)
    result = []
    for name in sorted(folded[0].keys() & folded[1].keys()):
        a, b = folded[0][name], folded[1][name]
        if len(a) == len(b) == 1:
            result.append((name, next(iter(a)), next(iter(b))))
    return result

def reference_alias_groups(original, proposed, states, limit):
    """Collect candidate aliases discovered on a separate optimized RTL copy."""
    targets = {}
    for name in sorted(original.keys() & proposed.keys()):
        old = original[name]
        if old in states:
            targets.setdefault(old, set()).add(proposed[name])
        if len(targets) > limit:
            raise ValueError('reference-alias proposal budget reached')
    groups = {}
    for old, values in targets.items():
        if len(values) == 1:
            groups.setdefault(next(iter(values)), set()).add(old)
    return groups

def state_output_aliases(model, states, limits):
    """Resolve bounded chains of explicit buffers/inverters to storage Q."""
    unary = {}
    for cell in model['cells'].values():
        typ = cell['type']
        if typ not in ('$not', '$_NOT_', '$pos', '$buf', '$_BUF_'):
            continue
        ports = cell['connections']
        if 'A' not in ports or 'Y' not in ports or len(ports['A']) != len(ports['Y']):
            continue
        for source, target in zip(ports['A'], ports['Y']):
            if isinstance(target, int):
                unary[target] = (source, int(typ in ('$not', '$_NOT_')))
    aliases, work = {}, 0
    for name, net in model['netnames'].items():
        if '$' in name or re.match(r'^_[0-9]+(?:\.|$)', name):
            continue
        bits = net['bits']
        for index, bit in enumerate(bits):
            current, parity, seen = bit, 0, set()
            for _ in range(128):
                work += 1
                if work > limits.max_connections:
                    raise ValueError('storage-output alias work budget reached')
                if current in states:
                    offset = net.get('offset', 0)
                    number = offset + len(bits) - 1 - index if net.get('upto', 0) else offset + index
                    key = name if len(bits) == 1 else f'{name}[{number}]'
                    aliases[key] = (current, parity)
                    break
                if current in seen or current not in unary:
                    break
                seen.add(current)
                current, inverted = unary[current]
                parity ^= inverted
            if len(aliases) > limits.max_bits:
                raise ValueError('storage-output alias name budget reached')
    return aliases

def load(path, top, limits):
    if path.stat().st_size > limits.max_json_bytes:
        raise ValueError('JSON size budget')
    return json.loads(path.read_text())['modules'][top]


def proposal_aliases(model, limits):
    """Names only; proposal cells are never used as the semantic reference.

    External optimization can introduce $adlatch in this disposable copy.
    Original storage/data/control functions still go through strict info().
    """
    aliases = {}
    for name, net in model.get('netnames', {}).items():
        if '$' in name or re.match(r'^_[0-9]+_(?:\.|$)', name):
            continue
        bits, offset = net['bits'], net.get('offset', 0)
        for index, bit in enumerate(bits):
            number = offset + len(bits) - 1 - index if net.get('upto', 0) else offset + index
            key = name if len(bits) == 1 else f'{name}[{number}]'
            if key in aliases and aliases[key] != bit:
                raise ValueError('ambiguous proposal alias')
            aliases[key] = bit
            if len(aliases) > limits.max_bits:
                raise ValueError('proposal alias budget reached')
    return aliases


def lower_async_latches(model, limits):
    """Express level-sensitive async reset as an ordinary transparent load.

    With reset priority, an asynchronous latch loads whenever reset OR enable
    is active, and loads reset_value when reset is active. This is an exact
    storage identity, unlike sampling an asynchronous control on a clock.
    """
    fresh = next_bit_id(model)
    cells = model['cells']
    count = 0

    def gate(typ, ports, width=1):
        nonlocal fresh
        out = list(range(fresh, fresh + width))
        fresh += width
        name = '$normalized_latch_' + str(fresh)
        if name in cells or len(cells) >= limits.max_cells or fresh > limits.max_bits * 16:
            raise ValueError('asynchronous latch lowering budget reached')
        ports = {**ports, 'Y': out}
        params = {'Y_WIDTH': format(width, 'b')}
        if typ == '$mux':
            params = {'WIDTH': format(width, 'b')}
        else:
            for port in ports:
                if port != 'Y':
                    params[port + '_WIDTH'] = format(len(ports[port]), 'b')
                    params[port + '_SIGNED'] = '0'
        cells[name] = {'type': typ, 'parameters': params, 'attributes': {},
                       'port_directions': {p: 'output' if p == 'Y' else 'input' for p in ports},
                       'connections': ports}
        return out

    for cell in list(cells.values()):
        if cell['type'] != '$adlatch':
            continue
        p, params = cell['connections'], cell['parameters']
        width = len(p['Q'])
        if (len(p['D']) != width or len(p['EN']) != 1 or len(p['ARST']) != 1
                or width > limits.max_state):
            raise ValueError('invalid asynchronous latch interface')
        enable, reset = p['EN'], p['ARST']
        for name in ('EN_POLARITY', 'ARST_POLARITY'):
            if intval(params, name) not in (0, 1):
                raise ValueError('invalid asynchronous latch polarity')
        if not intval(params, 'EN_POLARITY'):
            enable = gate('$not', {'A': enable})
        if not intval(params, 'ARST_POLARITY'):
            reset = gate('$not', {'A': reset})
        value = params['ARST_VALUE']
        if len(value) != width or any(v not in '01xz' for v in value):
            raise ValueError('invalid asynchronous latch reset value')
        data = gate('$mux', {'A': p['D'], 'B': list(value[::-1]), 'S': reset}, width)
        active = gate('$or', {'A': enable, 'B': reset})
        cell.update(type='$dlatch', parameters={'WIDTH': format(width, 'b'), 'EN_POLARITY': '1'},
                    port_directions={'D': 'input', 'EN': 'input', 'Q': 'output'},
                    connections={'D': data, 'EN': active, 'Q': p['Q']})
        count += 1
    return count

def info(m, limits):
    """Read declared storage interfaces; never infer or discard state bits."""
    cells = m['cells']
    if len(cells) > limits.max_cells:
        raise ValueError('cell budget reached')
    drivers = {}
    state = {}
    count = 0
    for name, c in cells.items():
        typ = c['type']
        ports = c['connections']
        dirs = c['port_directions']
        if typ in ('$assume', '$assert', '$cover', '$check', '$anyconst', '$anyseq', '$anyinit', '$allconst', '$allseq', '$initstate'):
            raise ValueError('unsupported formal source cell ' + typ)
        if typ.startswith('$mem') or typ.startswith('$_SR_') or typ in ('$sr', '$fsm') or not typ.startswith('$'):
            raise ValueError('unsupported state or unexpanded cell ' + typ)
        if any((x in typ.lower() for x in ['ff', 'latch'])) and typ not in SEQ:
            raise ValueError('unsupported state ' + typ)
        for pn, bits in ports.items():
            count += len(bits)
            if count > limits.max_connections:
                raise ValueError('arc budget')
            if dirs[pn] == 'output':
                for b in bits:
                    if isinstance(b, int):
                        if b in drivers and drivers[b] != name:
                            raise ValueError('multiple drivers')
                        drivers[b] = name
        if typ in SEQ:
            if len(ports['D']) != len(ports['Q']):
                raise ValueError('storage data/state widths differ')
            fine_ff = re.fullmatch(r'\$_DFF_([PN])(?:([PN])([01]))?_', typ)
            fine_latch = re.fullmatch(r'\$_DLATCH_([PN])_', typ)
            latch = typ == '$dlatch' or fine_latch is not None
            clock_name = 'C' if fine_ff else 'E' if fine_latch else 'EN' if latch else 'CLK'
            reset_name = 'R' if fine_ff and fine_ff[2] else 'ARST'
            if len(ports[clock_name]) != 1 or len(ports.get(reset_name, ['0'])) != 1:
                raise ValueError('storage clock/reset is not scalar')
            params = c['parameters']
            polarity = int((fine_ff or fine_latch)[1] == 'P') if fine_ff or fine_latch else intval(params, 'EN_POLARITY' if latch else 'CLK_POLARITY')
            reset_polarity = int(fine_ff[2] == 'P') if fine_ff and fine_ff[2] else intval(params, 'ARST_POLARITY', 1)
            reset_value = fine_ff[3] if fine_ff and fine_ff[3] else params.get('ARST_VALUE', '0' * len(ports['Q']))
            normalized_type = '$dlatch' if latch else '$adff' if typ == '$adff' or (fine_ff and fine_ff[2]) else '$dff'
            if polarity not in (0, 1) or reset_polarity not in (0, 1):
                raise ValueError('invalid storage control polarity')
            if not isinstance(reset_value, str) or len(reset_value) != len(ports['Q']) or any(v not in '01xz' for v in reset_value):
                raise ValueError('invalid storage reset value')
            for i, q in enumerate(ports['Q']):
                if not isinstance(q, int):
                    raise ValueError('non-net state')
                if q in state:
                    raise ValueError('multiple state drivers')
                state[q] = {
                    'cell': name, 'type': normalized_type,
                    'd': ports['D'][i], 'clock': ports[clock_name][0], 'pol': polarity,
                    'reset': ports.get(reset_name, ['0'])[0], 'reset_pol': reset_polarity,
                    'value': reset_value[::-1][i],
                }
    if len(state) > limits.max_state:
        raise ValueError('state-bit budget reached')
    inputs = {b for port in m['ports'].values() if port['direction'] == 'input' for b in port['bits']}
    if inputs & state.keys():
        raise ValueError('storage output also drives a primary input')
    aliases = {}
    init = {}
    for name, n in m.get('netnames', {}).items():
        bits = n['bits']
        iv = n.get('attributes', {}).get('init')
        if iv is not None and (not isinstance(iv, str) or len(iv) != len(bits) or any(v not in '01xz' for v in iv)):
            raise ValueError('invalid state initialization attribute')
        for i, b in enumerate(bits):
            if iv is not None and b in state:
                v = iv[::-1][i]
                if v in '01':
                    if b in init and init[b] != v:
                        raise ValueError('conflicting init')
                    init[b] = v
        if '$' in name or re.match(r'^_[0-9]+_(?:\.|$)', name):
            continue
        offset = n.get('offset', 0)
        upto = n.get('upto', 0)
        for i, b in enumerate(bits):
            if b in state or b in ('0', '1'):
                key = name if len(bits) == 1 else f'{name}[{(offset + len(bits) - 1 - i if upto else offset + i)}]'
                if key in aliases and aliases[key] != b:
                    raise ValueError('ambiguous state alias: ' + key)
                aliases[key] = b
                if len(aliases) > limits.max_bits:
                    raise ValueError('state-name budget reached')
    return (state, aliases, init)

def guard_controls(m, limits):
    """Require defined event controls for all defined inputs and state values.

    This conservative fan-in check may reject masked don't-cares. It must never
    silently complete an undefined clock or asynchronous reset to zero.
    """
    inputs = {b for p in m['ports'].values() if p['direction'] == 'input' for b in p['bits'] if isinstance(b, int)}
    drivers = {}
    for c in m['cells'].values():
        for name, bits in c['connections'].items():
            if c['port_directions'][name] == 'output':
                for b in bits:
                    if isinstance(b, int):
                        drivers[b] = c
    todo = [b for n, p in m['ports'].items() if n.startswith('__lec_control_') for b in p['bits']]
    seen = set()
    work = 0
    while todo:
        b = todo.pop()
        work += 1
        if work > limits.max_connections or len(seen) > limits.max_bits:
            raise ValueError('control cone budget')
        if b in ('0', '1'):
            continue
        if isinstance(b, str):
            raise ValueError('undefined clock/reset control cone')
        if b in inputs or b in seen:
            continue
        seen.add(b)
        if b not in drivers:
            raise ValueError('undriven clock/reset control')
        c = drivers[b]
        for name, bits in c['connections'].items():
            if c['port_directions'][name] == 'input':
                todo.extend(bits)

def prepare(src, d, top, yosys, seconds, memory_gib, limits, coarsen=False, keep_names=False,
            mapped_reference=False):
    """Propose a state relation and create data/control proof obligations."""
    start = time.monotonic()
    d.mkdir(parents=True, exist_ok=False)
    s = []
    for side in ['gold', 'gate']:
        s += [f"read_rtlil {equiv.quote(src / (side + '.il'))}"]
        if coarsen:
            s += [f"write_verilog -norename {equiv.quote(d / (side + '-coarse.v'))}", 'design -reset', f"read_verilog -sv {equiv.quote(d / (side + '-coarse.v'))}", f'hierarchy -top {equiv.object_name(top)}', 'proc']
        s += ['dffunmap', 'opt_clean', f"write_json {equiv.quote(d / (side + '.json'))}"]
        if side == 'gold':
            # This extra copy proposes duplicate-state aliases only. The
            # original gold.json remains the source of all proof obligations.
            s += ['opt -full', 'dffunmap', 'opt_clean',
                  f"write_json {equiv.quote(d / 'gold-proposal.json')}"]
        s += ['design -reset']
    (d / 'export.ys').write_text('\n'.join(s) + '\n')
    r = run.execute([yosys, '-Q', '-T', '-s', str(d / 'export.ys')], d, 'export', seconds=min(300, seconds), memory_gib=memory_gib, max_file_bytes=limits.max_json_bytes)
    run.save(d / 'export-result.json', r)
    if r['status'] != 'ok':
        raise ValueError('export failed')
    # Release the proposal model's heap before loading both proof models.
    alias_seconds = min(300, seconds - (time.monotonic() - start))
    if alias_seconds <= 0:
        raise ValueError('reference alias metadata deadline reached')
    alias_step = run.execute([sys.executable, str(Path(__file__).resolve()),
                              '--alias-metadata', str(d / 'gold-proposal.json'),
                              str(d / 'gold-proposal-aliases.json'), top,
                              json.dumps(limits.__dict__)], d, 'alias-metadata',
                             seconds=alias_seconds, memory_gib=min(4, memory_gib),
                             max_file_bytes=limits.max_json_bytes)
    run.save(d / 'alias-metadata-result.json', alias_step)
    if alias_step['status'] != 'ok':
        raise ValueError('reference alias metadata failed')
    proposed_aliases = run.read_json(d / 'gold-proposal-aliases.json')
    models = [load(d / (side + '.json'), top, limits) for side in ['gold', 'gate']]
    normalized_latches = [lower_async_latches(model, limits) for model in models]
    shapes = [{n: (p['direction'], len(p['bits']), p.get('offset', 0), p.get('upto', 0)) for n, p in m['ports'].items()} for m in models]
    if shapes[0] != shapes[1]:
        raise ValueError('original port interface mismatch')
    if any(kind == 'inout' for kind, *_ in shapes[0].values()):
        raise ValueError('bidirectional ports are not supported by the transition checker')
    if not any(kind == 'output' for kind, *_ in shapes[0].values()):
        raise ValueError('no observable output ports')
    if any((n.startswith('__lec_') for m in models for n in m['netnames'])) or any((n.startswith('$lec_') for m in models for n in m['cells'])):
        raise ValueError('reserved transition namespace collision')
    infos = [info(m, limits) for m in models]
    states = [x[0] for x in infos]
    aliases = [x[1] for x in infos]
    if mapped_reference:
        # Generated gate instance/pin IDs can move when a mapper changes a
        # combinational cell. They cannot propose register correspondence.
        # This filters names only: all state bits/data/controls remain in the
        # models, and every proposed relation still requires an external proof.
        aliases = [{name: bit for name, bit in side.items()
                    if not re.match(r'^[gn][0-9]+(?:\.|\[|$)', name)} for side in aliases]
    inits = [x[2] for x in infos]
    reference_proposals = reference_alias_groups(aliases[0], proposed_aliases, states[0], limits.max_bits)
    excluded_constants = set()
    for name in limits.constant_exclusions:
        if name not in aliases[0] or aliases[0][name] not in states[0]:
            raise ValueError('constant relation exclusion is not an original state: '+name)
        excluded_constants.add(aliases[0][name])
    parent = {}

    def find(x):
        if x not in parent:
            parent[x] = x
        for _ in range(limits.max_state * 2 + 4):
            if parent[x] == x:
                return x
            parent[x] = parent[parent[x]]
            x = parent[x]
        raise ValueError('union cycle')

    def join(a, b):
        a, b = (find(a), find(b))
        if a != b:
            parent[b] = a
    for side, st in enumerate(states):
        for b in st:
            find((side, b))
    for name in sorted(aliases[0].keys() & aliases[1].keys()):
        a, b = (aliases[0][name], aliases[1][name])
        if a not in states[0] and b not in states[1]:
            continue
        if a in states[0] and b in states[1]:
            join((0, a), (1, b))
        elif a in states[0] and b in ('0', '1') and a not in excluded_constants:
            # A proposal only: every constant needs its own initial and next
            # state proof. Optional false matches can be left unpaired below.
            join((0, a), ('c', b))
        elif b in states[1] and a in ('0', '1'):
            join((1, b), ('c', a))
    for name, b in aliases[1].items():
        a = aliases[0].get(name + '[0]')
        if b in states[1] and a in states[0]:
            join((0, a), (1, b))
    for name, a in aliases[0].items():
        b = aliases[1].get(name + '[0]')
        if a in states[0] and b in states[1]:
            join((0, a), (1, b))
    scope_pairs = []
    for name, a, b in anonymous_scope_pairs(aliases, states, limits.max_bits):
        if find((0, a)) != find((1, b)):
            join((0, a), (1, b))
            scope_pairs.append({'name': name, 'gold': a, 'gate': b})
    # Only attach previously unmatched reference state to one already matched
    # implementation state. Never merge two independent gate initial states.
    gate_roots = {find((1, b)) for b in states[1]}
    reference_pairs = []
    proposals = reference_proposals
    for value in sorted(proposals, key=str):
        members = proposals[value]
        if value in ('0', '1'):
            for bit in sorted(members):
                if find((0, bit)) not in gate_roots and bit not in excluded_constants:
                    join(('c', value), (0, bit))
                    reference_pairs.append({'gold': bit, 'constant': value})
            continue
        anchors = {find((0, bit)): bit for bit in sorted(members) if find((0, bit)) in gate_roots}
        if len(anchors) == 1:
            anchor = next(iter(anchors.values()))
            for bit in sorted(members):
                if find((0, bit)) not in gate_roots:
                    join((0, anchor), (0, bit))
                    reference_pairs.append({'gold': bit, 'anchor': anchor})
    groups = collections.defaultdict(list)
    for node in parent:
        groups[find(node)].append(node)
    groups = list(groups.values())
    groups.sort(key=lambda ns: sorted((str(n) for n in ns))[0])
    records = []
    unmatched = []
    for ns in groups:
        q = [[b for s, b in ns if s == side] for side in [0, 1]]
        constants = {b for s, b in ns if s == 'c'}
        if len(constants) > 1:
            raise ValueError('contradictory constant state relation')
        c = next(iter(constants), None)
        if c is None and (not q[0] or not q[1]):
            unmatched.extend(ns)
            continue
        if len(q[1]) > 1:
            raise ValueError('multiple independent gate states require a separate invariant proof')
        gi = [inits[1].get(b) for b in q[1]]
        ri = [inits[0].get(b) for b in q[0]]
        if c is not None:
            if any((x != c for x in gi)):
                raise ValueError('gate init does not cover constant relation')
            if any((x is not None and x != c for x in ri)):
                raise ValueError('gold init contradicts constant relation')
        else:
            gd = {'0', '1'} if gi[0] is None else {gi[0]}
            if any((x is not None and gd != {x} for x in ri)):
                raise ValueError('initial states incompatible')
        records.append({'gold': q[0], 'gate': q[1], 'constant': c})
    orphan_gold = {bit for side, bit in unmatched if side == 0}
    orphan_gate = {bit for side, bit in unmatched if side == 1}
    candidates = {}
    for name, (bit, polarity) in state_output_aliases(models[1], states[1], limits).items():
        if bit in orphan_gate:
            key = '.'.join(part for part in name.split('.') if not re.fullmatch(r'genblk[0-9]+', part))
            candidates.setdefault(key, set()).add((bit, polarity))
    by_gold = {}
    for name, bit in aliases[0].items():
        if bit in orphan_gold:
            key = '.'.join(part for part in name.split('.') if not re.fullmatch(r'genblk[0-9]+', part))
            by_gold.setdefault(bit, set()).update(candidates.get(key, ()))
    by_gate = {}
    for bit, choices in by_gold.items():
        if len(choices) == 1:
            gate_bit, polarity = next(iter(choices))
            by_gate.setdefault(gate_bit, {}).setdefault(polarity, []).append(bit)
    consumed, indirect_pairs = set(), []
    for gate_bit, polarities in sorted(by_gate.items()):
        if len(polarities) != 1:
            continue
        polarity, gold_bits = next(iter(polarities.items()))
        gate_init = inits[1].get(gate_bit)
        logical_init = str(int(gate_init) ^ polarity) if gate_init is not None else None
        allowed = {'0', '1'} if logical_init is None else {logical_init}
        if any(inits[0].get(bit) is not None and allowed != {inits[0][bit]} for bit in gold_bits):
            raise ValueError('initial states incompatible for indirect storage output')
        record = {'gold': sorted(gold_bits), 'gate': [gate_bit],
                  'constant': None, 'gate_inverted': bool(polarity)}
        records.append(record)
        indirect_pairs.append(record)
        consumed.add((1, gate_bit))
        consumed.update((0, bit) for bit in gold_bits)
    forgotten = [node for node in unmatched if tuple(node) not in consumed]
    for side, key in ((0, "gold"), (1, "gate")):
        accounted = [bit for record in records for bit in record[key]]
        accounted += [bit for owner, bit in forgotten if owner == side]
        if len(accounted) != len(set(accounted)) or set(accounted) != set(states[side]):
            raise ValueError("incomplete or duplicate storage-bit coverage")
    probes = []
    for gi, g in enumerate(records):
        if g['constant'] is None:
            pairs = [(r, g['gate'][0]) for r in g['gold']] + [(g['gold'][0], t) for t in g['gate'][1:]]
        else:
            pairs = [(r, None) for r in g['gold']] + [(None, t) for t in g['gate']]
        for j, (r, t) in enumerate(pairs):
            desc = [states[0][r] if r is not None else None, states[1][t] if t is not None else None]
            if g['constant'] is None and (desc[0]['type'] == '$dlatch') != (desc[1]['type'] == '$dlatch'):
                raise ValueError('latch vs FF kind mismatch')
            probes.append((gi, j, desc))
    controls = []
    seen_controls = set()
    reset_values_checked = 0
    for gi, j, desc in probes:
        if records[gi]['constant'] is not None:
            continue
        for field in ['clock', 'async']:
            pair = []
            for q in desc:
                pair.append((q['clock'], q['pol']) if field == 'clock' else (q['reset'], q['reset_pol']) if q['type'] == '$adff' else ('0', 1))
            if any((b in ('x', 'z') for b, p in pair)):
                raise ValueError('undefined clock/reset control')
            key = (field, tuple(pair))
            if key not in seen_controls:
                seen_controls.add(key)
                controls.append(key)
        rv = [q['value'] if q['type'] == '$adff' else '0' for q in desc]
        if records[gi].get('gate_inverted') and desc[1]['type'] == '$adff' and rv[1] in ('0', '1'):
            rv[1] = str(1 - int(rv[1]))
        if rv[0] not in ('x', 'z') and rv[0] != rv[1]:
            raise ValueError('incompatible asynchronous reset value')
        reset_values_checked += 1
    for side, m in enumerate(models):
        fresh = next_bit_id(m)
        replace = {}
        ports = m['ports']
        netnames = m.setdefault('netnames', {})
        for i, g in enumerate(records):
            bs = g['gold' if side == 0 else 'gate']
            if g['constant'] is not None:
                v = g['constant']
            else:
                v = fresh
                fresh += 1
                n = '__lec_state_' + str(i)
                if n in ports:
                    raise ValueError('state input namespace collision')
                ports[n] = {'direction': 'input', 'bits': [v]}
                netnames[n] = {'hide_name': 0, 'bits': [v], 'attributes': {}}
                if side == 1 and g.get('gate_inverted'):
                    inverted = fresh
                    fresh += 1
                    m['cells']['$lec_state_inv_' + str(inverted)] = {
                        'hide_name': 1, 'type': '$not',
                        'parameters': {'A_SIGNED': '0', 'A_WIDTH': '1', 'Y_WIDTH': '1'},
                        'attributes': {}, 'port_directions': {'A': 'input', 'Y': 'output'},
                        'connections': {'A': [v], 'Y': [inverted]}}
                    v = inverted
            for b in bs:
                replace[b] = v
        for fi, (owner, b) in enumerate(forgotten):
            v = fresh
            fresh += 1
            n = f'__lec_unmatched_{fi}'
            ports[n] = {'direction': 'input', 'bits': [v]}
            netnames[n] = {'hide_name': 0, 'bits': [v], 'attributes': {}}
            if owner == side:
                replace[b] = v

        def bit(b):
            return replace.get(b, normbit(b))
        # An asserted asynchronous reset determines the visible Q value even
        # for an otherwise arbitrary stored state. This is cell semantics, not
        # a reset-sequence assumption. Keep the independent raw state input.
        for q, storage in states[side].items():
            if storage['type'] != '$adff':
                continue
            reset = storage['reset']
            if not storage['reset_pol']:
                inv = fresh
                fresh += 1
                m['cells']['$lec_visible_reset_inv_'+str(inv)] = {
                    'type':'$not','parameters':{'A_SIGNED':'0','A_WIDTH':'1','Y_WIDTH':'1'},
                    'attributes':{},'port_directions':{'A':'input','Y':'output'},
                    'connections':{'A':[reset],'Y':[inv]}}
                reset = inv
            visible = fresh
            fresh += 1
            m['cells']['$lec_visible_reset_'+str(visible)] = {
                'type':'$mux','parameters':{'WIDTH':'1'},'attributes':{},
                'port_directions':{'A':'input','B':'input','S':'input','Y':'output'},
                'connections':{'A':[replace[q]],'B':[storage['value']],'S':[reset],'Y':[visible]}}
            replace[q] = visible
            if len(m['cells']) > limits.max_cells:
                raise ValueError('asynchronous state visibility cell budget reached')
        for cn, c in list(m['cells'].items()):
            if c['type'] in SEQ:
                del m['cells'][cn]
            else:
                for pn, bs in c['connections'].items():
                    c['connections'][pn] = [bit(b) for b in bs]
        for n, p in list(ports.items()):
            p['bits'] = [bit(b) for b in p['bits']]
        for n, w in list(netnames.items()):
            w['bits'] = [bit(b) for b in w['bits']]
            w.get('attributes', {}).pop('init', None)

        def output(name, b):
            ports[name] = {'direction': 'output', 'bits': [b]}
            netnames[name] = {'hide_name': 0, 'bits': [b], 'attributes': {}}
        inv_cache = {}
        and_cache = {}

        def control(b, pol):
            nonlocal fresh
            b = bit(b)
            if pol:
                return b
            if b in ('0', '1'):
                return '1' if b == '0' else '0'
            if isinstance(b, str):
                return 'x'
            if b not in inv_cache:
                v = fresh
                fresh += 1
                inv_cache[b] = v
                m['cells']['$lec_inv_' + str(v)] = {'hide_name': 1, 'type': '$not', 'parameters': {'A_SIGNED': '0', 'A_WIDTH': '1', 'Y_WIDTH': '1'}, 'attributes': {}, 'port_directions': {'A': 'input', 'Y': 'output'}, 'connections': {'A': [b], 'Y': [v]}}
            return inv_cache[b]

        def conjunction(a, b):
            nonlocal fresh
            if a == '0' or b == '0':
                return '0'
            if a == '1':
                return b
            if b == '1' or a == b:
                return a
            key = tuple(sorted((a, b), key=str))
            if key not in and_cache:
                v = fresh
                fresh += 1
                and_cache[key] = v
                m['cells']['$lec_and_' + str(v)] = {
                    'hide_name': 1, 'type': '$and',
                    'parameters': {'A_SIGNED': '0', 'B_SIGNED': '0',
                                   'A_WIDTH': '1', 'B_WIDTH': '1', 'Y_WIDTH': '1'},
                    'attributes': {},
                    'port_directions': {'A': 'input', 'B': 'input', 'Y': 'output'},
                    'connections': {'A': [a], 'B': [b], 'Y': [v]}}
            return and_cache[key]

        def active_event(q):
            active = control(q['clock'], q['pol'])
            if q['type'] == '$adff':
                active = conjunction(active, control(q['reset'], 1 - q['reset_pol']))
            return active

        for gi, j, desc in probes:
            q = desc[side]
            prefix = '__lec_next_' + str(gi) + '_' + str(j)
            if records[gi]['constant'] is not None:
                c = records[gi]['constant']
                output(prefix, conjunction(control(q['d'], int(c == '0')), active_event(q)) if q else '0')
                if q and q['type'] == '$adff' and q['value'] != c and not (side == 0 and q['value'] in ('x', 'z')):
                    raise ValueError('reset violates constant state relation')
            else:
                data = control(q['d'], 0) if side == 1 and records[gi].get('gate_inverted') else bit(q['d'])
                output(prefix + '_D', conjunction(data, active_event(q)))
        for ci, (field, pair) in enumerate(controls):
            b, pol = pair[side]
            output(f'__lec_control_{ci}_{field}', control(b, pol))
        port_names = {n: netnames.get(n, {'hide_name': 0, 'bits': p['bits'], 'attributes': {}}) for n, p in ports.items()}
        m['netnames'] = {**netnames, **port_names} if keep_names else port_names
        m['attributes'] = {'top': '1'}
        with (d / ('gold-comb.json' if side == 0 else 'gate-comb.json')).open('w') as f:
            json.dump({'modules': {top: m}}, f, separators=(',', ':'))
    for m in models:
        guard_controls(m, limits)
    summary = {'coverage': list(map(len, states)), 'groups': records, 'forgotten_state': forgotten, 'probes': len(probes), 'unique_control_pairs': len(controls), 'reset_values_checked': reset_values_checked, 'initial_relation_covers_all_gate_initial_states': True, 'all_state_mapped_or_universally_quantified': True, 'clock_reset_cones_defined': True, 'anonymous_scope_pairs': scope_pairs, 'reference_alias_proposals': reference_pairs, 'reference_alias_proposal_sha256': run.digest(d / 'gold-proposal-aliases.json'), 'indirect_state_pairs': indirect_pairs, 'next_data_scope': 'active clock/latch event outside asynchronous reset; all control functions checked separately'}
    summary['asynchronous_latches_lowered'] = normalized_latches
    summary['asynchronous_state_visibility'] = 'reset value while reset is active; stored state otherwise arbitrary'
    summary['unpaired_constant_candidates'] = [{'name':name,'gold':aliases[0][name]} for name in limits.constant_exclusions]
    run.save(d / 'state-relation.json', summary)
    return summary


METHOD = "abc-inductive-transition-priority-v5"
DUAL_METHOD = 'abc-inductive-transition-dual-xprop-v6'
METHODS = {"abc-inductive-transition-v1", "abc-inductive-transition-xprop-v2", "abc-inductive-transition-priority-v4", METHOD, DUAL_METHOD}
_TOTAL_CELLS = {
    "$and", "$or", "$not", "$xor", "$xnor", "$mux", "$pos", "$mul",
    "$add", "$sub", "$neg", "$shl", "$shr", "$sshl", "$sshr", "$shift",
    "$lt", "$le", "$gt", "$ge",
    "$logic_not", "$logic_and", "$logic_or", "$reduce_and", "$reduce_or",
    "$reduce_xor", "$reduce_xnor", "$reduce_bool", "$eq", "$ne", "$eqx", "$nex",
}


def aig_interface(path, limits, include_graph=False):
    """Read a bounded AIGER symbol table without interpreting its functions."""
    if path.stat().st_size > limits.max_json_bytes:
        raise ValueError("AIG file budget reached")
    with path.open("rb") as stream:
        header = stream.readline(4096).decode().strip()
        fields = header.split()
        if len(fields) != 6 or fields[0] != "aig":
            raise ValueError("expected a plain combinational binary AIG")
        maximum, inputs, latches, outputs, gates = map(int, fields[1:])
        if latches or min(maximum, inputs, outputs, gates) < 0 or gates > limits.max_connections:
            raise ValueError("invalid or sequential AIG interface")
        if maximum != inputs + gates:
            raise ValueError("AIG node numbering is not canonical")
        literals = []
        for _ in range(outputs):
            literal = stream.readline(4096)
            if not literal.strip().isdigit():
                raise ValueError("invalid AIG output literal")
            value = int(literal)
            if value > 2 * maximum + 1:
                raise ValueError("AIG output literal is out of range")
            literals.append(value)
        graph_start = stream.tell()
        for _ in range(2 * gates):
            for _ in range(10):
                byte = stream.read(1)
                if not byte:
                    raise ValueError("truncated AIG data")
                if byte[0] < 128:
                    break
            else:
                raise ValueError("AIG integer budget reached")
        graph_end = stream.tell()
        names = {"i": {}, "o": {}}
        while line := stream.readline(16385):
            if len(line) > 16384:
                raise ValueError("AIG symbol budget reached")
            if line.rstrip() == b"c":
                break
            match = re.fullmatch(rb"([io])(\d+) (.*)\n?", line.rstrip(b"\n"))
            if not match:
                raise ValueError("unexpected AIG symbol record")
            kind, index, name = match[1].decode(), int(match[2]), match[3].decode()
            if index in names[kind]:
                raise ValueError("duplicate AIG symbol index")
            names[kind][index] = name
    if set(names["i"]) != set(range(inputs)) or set(names["o"]) != set(range(outputs)):
        raise ValueError("incomplete AIG port symbols")
    result = {"header": header, "inputs": [names["i"][i] for i in range(inputs)],
              "outputs": [names["o"][i] for i in range(outputs)]}
    if include_graph:
        with path.open("rb") as stream:
            stream.seek(graph_start)
            graph = stream.read(graph_end - graph_start)
        if len(graph) != graph_end - graph_start:
            raise ValueError("truncated AIG graph")
        result.update({"maximum": maximum, "gates": gates, "graph": graph,
                       "literals": literals, "graph_sha256": hashlib.sha256(graph).hexdigest()})
    return result


def project_outputs(image, first, count, path, limits):
    """Select outputs while copying every input and AND byte unchanged.

    This is a literal projection, not ABC's automatic partitioning. Every
    original output is covered by a recorded, nonoverlapping proof range.
    """
    if first < 0 or count <= 0 or count > limits.max_batch_outputs or first + count > len(image["outputs"]):
        raise ValueError("invalid AIG output projection")
    header = f"aig {image['maximum']} {len(image['inputs'])} 0 {count} {image['gates']}\n".encode()
    with path.open("wb") as stream:
        stream.write(header)
        for literal in image["literals"][first:first + count]:
            stream.write(f"{literal}\n".encode())
        if stream.write(image["graph"]) != len(image["graph"]):
            raise ValueError("incomplete AIG graph write")
        for index, name in enumerate(image["inputs"]):
            stream.write(f"i{index} {name}\n".encode())
        for index, name in enumerate(image["outputs"][first:first + count]):
            stream.write(f"o{index} {name}\n".encode())
        stream.write(b"c\nExplicit output projection; AND graph and inputs unchanged.\n")


def ordered_port_names(ports, direction):
    result = []
    for name, port in sorted(ports.items()):
        if port["direction"] != direction:
            continue
        width = len(port["bits"])
        for index in range(width):
            bit = port.get("offset", 0) + (width - 1 - index if port.get("upto", 0) else index)
            result.append(name if width == 1 else f"{name}[{bit}]")
    if len(set(result)) != len(result):
        raise ValueError("colliding AIG port names")
    return result


def cec_equivalent(text):
    return (bool(re.search(r"^Networks are equivalent(?:\.| after structural hashing\.)", text, re.M))
            and not re.search(r"NOT EQUIVALENT|UNDECIDED|Verification failed", text, re.I))


def cec_command(limits, seconds, gold, gate):
    if limits.cec_engine == 'gia':
        # &cec uses positional interfaces directly. Its -n means a different
        # (naive SAT) algorithm, so do not copy legacy cec's -n option here.
        prefix = f'&cec -T {max(1, int(seconds))} -C 2000000'
    else:
        prefix = f'cec -n -T {max(1, int(seconds))} -C 2000000 -I 1000000000'
    return prefix + f' {equiv.quote(gold)} {equiv.quote(gate)};'


def refinement_wrapper(ports, top, gate=False, partial_gate=False):
    """Connect external xprop value/mask ports with fully defined inputs.

    Both sides compare value & ~reference_unknown. Thus a defined RTL bit is
    checked exactly, while an undefined RTL bit permits either Boolean value.
    No input assumptions or hand-written Verilog X propagation rules are used.
    """
    def ident(name):
        equiv.object_name(name)
        return '\\' + name + ' '

    if top in ('__lec_reference', '__lec_implementation'):
        raise ValueError('reserved refinement module name')
    constraints = {name: name + '__lec_target_undefined' for name, port in ports.items()
                   if partial_gate and port['direction'] == 'output'}
    if set(constraints.values()) & ports.keys():
        raise ValueError('definedness output namespace collision')
    lines = ['module ' + ident(top) + '(' + ','.join(ident(n) for n in [*ports, *constraints.values()]) + ');']
    reference, implementation = [], []
    for index, (name, port) in enumerate(ports.items()):
        width = len(port['bits'])
        if width <= 0 or port['direction'] not in ('input', 'output'):
            raise ValueError('unsupported refinement port')
        offset = port.get('offset', 0)
        span = ((offset, offset + width - 1) if port.get('upto', 0)
                else (offset + width - 1, offset))
        declaration = f'[{span[0]}:{span[1]}] ' if width > 1 else ''
        lines.append(port['direction'] + ' ' + declaration + ident(name) + ';')
        if port['direction'] == 'input':
            reference.append(f'.{ident(name + "_d")}({ident(name)})')
            implementation.append(f'.{ident(name + "_d" if partial_gate else name)}({ident(name)})')
        else:
            mask, value = f'__lec_mask_{index}', f'__lec_value_{index}'
            lines.extend([f'wire [{width - 1}:0] {mask};', f'wire [{width - 1}:0] {value};'])
            reference.append(f'.{ident(name + "_x")}({mask})')
            if gate:
                implementation.append(f'.{ident(name + "_d" if partial_gate else name)}({value})')
                if partial_gate:
                    gate_mask = f'__lec_gate_mask_{index}'
                    lines.append(f'wire [{width - 1}:0] {gate_mask};')
                    implementation.append(f'.{ident(name + "_x")}({gate_mask})')
            else:
                reference.append(f'.{ident(name + "_d")}({value})')
            lines.append(f'assign {ident(name)} = {value} & ~{mask};')
            if partial_gate:
                lines.append('output ' + declaration + ident(constraints[name]) + ';')
                expression = f'{gate_mask} & ~{mask}' if gate else f"{width}'b0"
                lines.append(f'assign {ident(constraints[name])} = {expression};')
    lines.append('__lec_reference reference_instance (' + ','.join(reference) + ');')
    if gate:
        lines.append('__lec_implementation implementation_instance (' + ','.join(implementation) + ');')
    lines.append('endmodule')
    return '\n'.join(lines) + '\n'


def known_reference_wrapper(ports, module_name='__lec_reference', encoded_name='__lec_encoded', total_boolean=False):
    """Tie every encoded input mask to zero before the large AIG lowering."""
    def ident(name):
        equiv.object_name(name)
        return '\\' + name + ' '
    declarations, connections, names, assignments = [], [], [], []
    for name, port in ports.items():
        width = len(port['bits'])
        for suffix in ('_d',) if port['direction'] == 'input' else ('_d', '_x'):
            signal = ident(name + suffix)
            names.append(signal)
            declarations.append(f"{port['direction']} [{width - 1}:0] {signal};")
            if total_boolean:
                if suffix == '_d':
                    connections.append(f'.{ident(name)}({signal})')
                else:
                    assignments.append(f'assign {signal} = {width}\'b0;')
            else:
                connections.append(f'.{signal}({signal})')
        if port['direction'] == 'input' and not total_boolean:
            connections.append(f'.{ident(name + "_x")}({width}\'b0)')
    return ('module ' + module_name + ' (' + ','.join(names) + ');\n'
            + '\n'.join(declarations + assignments) + '\n' + encoded_name + ' encoded ('
            + ','.join(connections) + ');\nendmodule\n')


def guard_total_implementation(model):
    """Check the already bounded model without serializing a second JSON copy.

    This scan adds O(1) state to the model's existing O(cells + connections)
    storage. The later external cleanup may only remove unused logic; external
    SCC/driver checks still run before lowering either proof interface.
    """
    for cell in model['cells'].values():
        typ = cell['type']
        if typ not in _TOTAL_CELLS and not re.fullmatch(r'\$_(?:NOT|BUF|AND|NAND|OR|NOR|XOR|XNOR|MUX|NMUX|ANDNOT|ORNOT)_', typ):
            raise ValueError('unverified implementation cell totality: ' + typ)
        if any(bit in ('x', 'z') for bits in cell['connections'].values() for bit in bits):
            raise ValueError('implementation has an undefined combinational value')
    for port in model['ports'].values():
        if any(bit in ('x', 'z') for bit in port['bits']):
            raise ValueError('implementation has an undefined output')


def total_boolean_reference(model, limits):
    """Recognize a fully driven Boolean graph, without optimizing its values.

    This O(connections) scan needs two bounded bit sets, not paths or cones.
    External SCC and driver checks must additionally succeed before the
    zero-mask encoding is accepted. Partial word operations fall back to xprop.
    """
    drivers, used = set(), set()
    def drive(bits):
        for bit in bits:
            if type(bit) is not int or bit in drivers:
                return False
            drivers.add(bit)
        return True
    def consume(bits):
        for bit in bits:
            if type(bit) is int:
                used.add(bit)
            elif bit not in ('0', '1'):
                return False
        return True
    for port in model['ports'].values():
        if port['direction'] == 'input':
            if not drive(port['bits']): return False
        elif port['direction'] != 'output' or not consume(port['bits']):
            return False
    for cell in model['cells'].values():
        if cell['type'] not in ('$and', '$or', '$not', '$xor', '$xnor', '$mux', '$pos') and not re.fullmatch(
                r'\$_(?:NOT|BUF|AND|NAND|OR|NOR|XOR|XNOR|MUX|NMUX|ANDNOT|ORNOT)_', cell['type']):
            return False
        if set(cell['connections']) != set(cell['port_directions']): return False
        for name, bits in cell['connections'].items():
            if cell['port_directions'][name] == 'output':
                if not drive(bits): return False
            elif cell['port_directions'][name] != 'input' or not consume(bits):
                return False
        if len(drivers) > limits.max_bits or len(used) > limits.max_bits:
            return False
    return used <= drivers


def compare(directory, output, top, yosys, abc, seconds, memory_gib, limits, partial_gate=False):
    """Prove defined RTL output/state bits against a fully defined gate function.

    Driver checks precede completion: a lost storage driver cannot be silently
    replaced by zero. External xprop encodes reference data undefinedness;
    clock/reset cones were separately checked for definedness.
    """
    start = time.monotonic()
    output.mkdir(parents=True, exist_ok=False)
    interfaces = []
    expected_names = None
    constant_dontcare = {}
    original_ports = None
    total_reference = False
    for side in ("gold", "gate"):
        model = load(directory / f"{side}-comb.json", top, limits)
        guard_controls(model, limits)
        if side == "gold":
            constant_dontcare = {
                name: [index for index, bit in enumerate(port["bits"]) if bit in ("x", "z")]
                for name, port in model["ports"].items() if port["direction"] == "output"
            }
            constant_dontcare = {name: bits for name, bits in constant_dontcare.items() if bits}
        # A literal undefined reference output is an unconditional don't-care.
        # Discharge that bit on both proof interfaces; never mask a merely
        # dynamic value or an undefined implementation bit with a defined RTL bit.
        for name, indices in constant_dontcare.items():
            bits = list(model["ports"][name]["bits"])
            for index in indices:
                bits[index] = "0"
            model["ports"][name]["bits"] = bits
            model["netnames"][name]["bits"] = list(bits)
        if side == 'gate' and not partial_gate:
            guard_total_implementation(model)
        if side == 'gold':
            total_reference = total_boolean_reference(model, limits)
        model["ports"] = dict(sorted(model["ports"].items()))
        if side == "gold":
            original_ports = model["ports"]
        interfaces.append({name: (port["direction"], len(port["bits"]))
                           for name, port in model["ports"].items()})
        names = {kind: ordered_port_names(model["ports"], direction)
                 for kind, direction in (("inputs", "input"), ("outputs", "output"))}
        if expected_names is None:
            expected_names = names
        elif names != expected_names:
            raise ValueError("transition port order differs")
        with (output / f"{side}-ordered.json").open("w") as stream:
            json.dump({"modules": {top: model}}, stream, separators=(",", ":"))
        del model
        gc.collect()
    if interfaces[0] != interfaces[1]:
        raise ValueError("transition comparison interfaces differ")
    comparison_ports = dict(original_ports)
    if partial_gate:
        for name, port in original_ports.items():
            if port['direction'] == 'output':
                extra = name + '__lec_target_undefined'
                if extra in comparison_ports:
                    raise ValueError('definedness output namespace collision')
                comparison_ports[extra] = dict(port)
        expected_names = {kind: ordered_port_names(comparison_ports, direction)
                          for kind, direction in (('inputs', 'input'), ('outputs', 'output'))}
    prepared = []
    reference_path = output / "reference-xprop.il"
    known_wrapper = output / 'reference-defined-inputs.v'
    known_wrapper.write_text(known_reference_wrapper(original_ports, total_boolean=total_reference))
    encoding = ([] if total_reference else ['setundef -undriven -undef']) + [
        'scc -expect 0', 'check -assert']
    if not total_reference:
        encoding += ['xprop -split-ports -required -formal']
    encode_script = output / "reference-xprop.ys"
    encode_script.write_text('\n'.join([
        f'read_json {equiv.quote(output / "gold-ordered.json")}',
        f'hierarchy -top {equiv.object_name(top)}', 'opt_clean -purge',
        # An undriven RTL data wire is explicitly undefined. Do not invent a
        # Boolean input or silently make it zero; the X mask owns this choice.
        *encoding,
        f'rename {equiv.object_name(top)} __lec_encoded',
        f'read_verilog {equiv.quote(known_wrapper)}',
        'hierarchy -check -top __lec_reference', 'flatten', 'opt -fast', 'opt_clean -purge',
        'scc -expect 0', 'check -assert',
        f'write_rtlil {equiv.quote(reference_path)}', 'blackbox',
        f'write_json {equiv.quote(output / "reference-interface.json")}',
    ]) + '\n')
    remaining = seconds - (time.monotonic() - start)
    if remaining <= 0:
        raise ValueError('reference encoding deadline reached')
    encoded = run.execute([yosys, '-Q', '-T', '-s', str(encode_script)], output,
                          'reference-xprop', seconds=remaining, memory_gib=memory_gib,
                          max_file_bytes=limits.max_json_bytes)
    prepared.append(encoded)
    run.save(output / 'reference-xprop-result.json', encoded)
    if encoded['status'] != 'ok':
        raise ValueError('reference definedness encoding failed')
    if total_reference:
        run.save(output / 'reference-totality.json', {
            'model_sha256': run.digest(output / 'gold-ordered.json'),
            'fully_driven_boolean_cells': True, 'external_scc_and_driver_checks': True,
            'input_unknown_masks': 'all zero; no input/reset value assumptions',
            'encoding_log_sha256': run.digest(output / 'reference-xprop.stdout')})
    encoded_interface = load(output / 'reference-interface.json', '__lec_reference', limits)
    expected_encoded = {name + suffix: (port['direction'], len(port['bits']))
                        for name, port in original_ports.items()
                        for suffix in (('_d',) if port['direction'] == 'input' else ('_d', '_x'))}
    actual_encoded = {name: (port['direction'], len(port['bits']))
                      for name, port in encoded_interface['ports'].items()}
    if expected_encoded != actual_encoded or encoded_interface['cells']:
        raise ValueError('reference value/unknown interface mismatch')
    del encoded_interface
    implementation_path = None
    if partial_gate:
        implementation_path = output / 'implementation-xprop.il'
        known_gate = output / 'implementation-defined-inputs.v'
        known_gate.write_text(known_reference_wrapper(original_ports, '__lec_implementation', '__lec_gate_encoded'))
        gate_script = output / 'implementation-xprop.ys'
        gate_script.write_text('\n'.join([
            f'read_json {equiv.quote(output / "gate-ordered.json")}',
            f'hierarchy -top {equiv.object_name(top)}', 'opt_clean -purge',
            'setundef -undriven -undef', 'scc -expect 0', 'check -assert',
            'xprop -split-ports -required -formal',
            f'rename {equiv.object_name(top)} __lec_gate_encoded',
            f'read_verilog {equiv.quote(known_gate)}',
            'hierarchy -check -top __lec_implementation', 'flatten', 'opt -fast', 'opt_clean -purge',
            'scc -expect 0', 'check -assert',
            f'write_rtlil {equiv.quote(implementation_path)}', 'blackbox',
            f'write_json {equiv.quote(output / "implementation-interface.json")}',
        ]) + '\n')
        remaining = seconds - (time.monotonic() - start)
        if remaining <= 0:
            raise ValueError('implementation encoding deadline reached')
        encoded_gate = run.execute([yosys, '-Q', '-T', '-s', str(gate_script)], output,
                                   'implementation-xprop', seconds=remaining, memory_gib=memory_gib,
                                   max_file_bytes=limits.max_json_bytes)
        prepared.append(encoded_gate)
        if encoded_gate['status'] != 'ok':
            raise ValueError('implementation definedness encoding failed')
        encoded_interface = load(output / 'implementation-interface.json', '__lec_implementation', limits)
        actual = {name: (port['direction'], len(port['bits']))
                  for name, port in encoded_interface['ports'].items()}
        if expected_encoded != actual or encoded_interface['cells']:
            raise ValueError('implementation value/unknown interface mismatch')
        del encoded_interface
    for side in ("gold", "gate"):
        # Use a fresh frontend process per side. A design reset can leave large
        # allocator arenas resident; retaining both sides exceeded the VexRiscv
        # physical-memory budget despite the graphs themselves fitting.
        wrapper = output / f'{side}-refinement.v'
        wrapper.write_text(refinement_wrapper(original_ports, top, gate=side == 'gate', partial_gate=partial_gate))
        script = []
        if side == "gate":
            if partial_gate:
                script += [f'read_rtlil {equiv.quote(implementation_path)}']
            else:
                script += [f"read_json {equiv.quote(output / 'gate-ordered.json')}",
                       f"hierarchy -top {equiv.object_name(top)}", 'opt_clean -purge',
                       'scc -expect 0', 'check -assert',
                       f"rename {equiv.object_name(top)} __lec_implementation"]
        script += [f'read_rtlil {equiv.quote(reference_path)}',
                   f'read_verilog {equiv.quote(wrapper)}',
                   f'hierarchy -check -top {equiv.object_name(top)}',
                   'flatten', 'opt_clean -purge', 'scc -expect 0', 'check -assert']
        # Verilog casts often widen a 16-bit multiplier to its 32/35-bit
        # result width. Reduce redundant sign/zero extensions before Boolean
        # lowering so the external solver need not rediscover that arithmetic
        # identity inside a much larger multiplier.
        script += ["wreduce", "alumacc", "techmap", "opt -fast", "aigmap", "opt_clean"]
        script += [f"write_aiger -symbols {equiv.quote(output / f'{side}.aig')}"]
        label = f"prepare-{side}"
        script_path = output / f"{label}.ys"
        script_path.write_text("\n".join(script) + "\n")
        preparation_time = seconds - (time.monotonic() - start)
        if preparation_time <= 0:
            raise ValueError("transition preparation deadline reached")
        result = run.execute([yosys, "-Q", "-T", "-s", str(script_path)],
                             output, label, seconds=preparation_time, memory_gib=memory_gib,
                             max_file_bytes=limits.max_json_bytes)
        prepared.append(result)
        run.save(output / f"{label}-result.json", result)
        if result["status"] != "ok":
            raise ValueError(f"transition {side} Boolean model preparation failed")
    expected_inputs = sum(width for direction, width in interfaces[0].values() if direction == "input")
    expected_outputs = sum(len(port['bits']) for port in comparison_ports.values() if port['direction'] == 'output')
    if expected_outputs <= 0:
        raise ValueError("empty transition comparison")
    headers = []
    actual_interfaces = []
    for side in ("gold", "gate"):
        actual_interface = aig_interface(output / f"{side}.aig", limits, include_graph=True)
        header = actual_interface["header"]
        fields = header.split()
        if len(fields) < 6 or fields[0] != "aig" or int(fields[3]) != 0:
            raise ValueError("transition comparison is not purely combinational")
        if int(fields[2]) != expected_inputs or int(fields[4]) != expected_outputs:
            raise ValueError("AIG export omitted or added interface bits")
        for kind in ("inputs", "outputs"):
            actual = actual_interface[kind]
            if len(set(actual)) != len(actual) or set(actual) != set(expected_names[kind]):
                raise ValueError("AIG port names omitted, duplicated or changed")
        actual_interfaces.append(actual_interface)
        headers.append(header)
    if any(actual_interfaces[0][kind] != actual_interfaces[1][kind] for kind in ("inputs", "outputs")):
        raise ValueError("gold and gate AIG port order differs")
    run.save(output / "aig-interface.json", {kind: actual_interfaces[0][kind] for kind in ("inputs", "outputs")})
    batches, checked_steps, covered = [], [], 0
    full_attempt = None
    aggregate_log = output / "cec.stdout"
    with aggregate_log.open("w") as combined:
        try_batches = True
        if limits.whole_attempt_s:
            remaining = seconds - (time.monotonic() - start)
            budget = min(remaining, limits.whole_attempt_s)
            if budget <= 0:
                raise ValueError("transition proof deadline reached")
            command = cec_command(limits, budget, output / 'gold.aig', output / 'gate.aig')
            checked = run.execute([abc, "-c", command], output, "cec-whole", seconds=budget,
                                  memory_gib=memory_gib, max_file_bytes=128 * 1024**2)
            checked_steps.append(checked)
            text = (output / "cec-whole.stdout").read_text()
            combined.write(f"All outputs [0, {expected_outputs})\n{text}\n")
            passed = checked["status"] == "ok" and cec_equivalent(text)
            full_attempt = {"proven": passed, "status": checked["status"],
                            "log_sha256": run.digest(output / "cec-whole.stdout")}
            if passed:
                covered = expected_outputs
                batches.append({"first": 0, "count": expected_outputs, "proven": True,
                                "log_sha256": full_attempt["log_sha256"], "whole_graph": True})
                try_batches = False
            elif re.search(r"NOT EQUIVALENT|Verification failed", text, re.I):
                try_batches = False
        for first in (range(0, expected_outputs, limits.max_batch_outputs) if try_batches else ()):
            count = min(limits.max_batch_outputs, expected_outputs - first)
            remaining = seconds - (time.monotonic() - start)
            if remaining <= 0:
                raise ValueError("transition proof deadline reached")
            for side, image in zip(("gold", "gate"), actual_interfaces):
                project_outputs(image, first, count, output / f"{side}-batch.aig", limits)
            command = cec_command(limits, remaining, output / 'gold-batch.aig', output / 'gate-batch.aig')
            label = f"cec-{first:06d}-{first + count:06d}"
            checked = run.execute([abc, "-c", command], output, label, seconds=remaining,
                                  memory_gib=memory_gib, max_file_bytes=128 * 1024**2)
            checked_steps.append(checked)
            text = (output / f"{label}.stdout").read_text()
            combined.write(f"Outputs [{first}, {first + count})\n{text}\n")
            passed = checked["status"] == "ok" and cec_equivalent(text)
            batches.append({"first": first, "count": count, "proven": passed,
                            "log_sha256": run.digest(output / f"{label}.stdout")})
            if not passed:
                break
            covered += count
    equivalent = covered == expected_outputs
    result = {"equivalent": equivalent, "steps": [*prepared, *checked_steps],
              "inputs": expected_inputs, "outputs": expected_outputs, "aiger_headers": headers,
              "gold_aiger_sha256": run.digest(output / "gold.aig"),
              "gate_aiger_sha256": run.digest(output / "gate.aig"),
              "cec_log_sha256": run.digest(output / "cec.stdout"),
              "gold_data_completion": "external xprop value/unknown encoding; defined RTL bits compared exactly",
              "reference_unknowns_encoded": True, "refinement_masks_checked": True,
              "reference_xprop_sha256": run.digest(reference_path),
              "reference_input_unknown_masks": "all zero; no input/reset assumptions",
              "implementation_function_total": not partial_gate, "clock_reset_cones_defined": True,
              "implementation_unknowns_encoded": partial_gate,
              "implementation_definedness_checked": partial_gate,
              "definedness_refinement_checked": partial_gate,
              "all_output_bits_compared": equivalent, "port_order_verified": True,
              "cec_matching": "verified positional", "cec_partitioning": False,
              "cec_engine": "ABC &cec" if limits.cec_engine == 'gia' else 'ABC cec -n',
              "reference_constant_dontcare": constant_dontcare,
              "explicit_batches": batches, "outputs_proven": covered,
              "whole_attempt": full_attempt,
              "projection_graphs_unchanged": True,
              "graph_sha256": [image["graph_sha256"] for image in actual_interfaces]}
    result['reference_encoding'] = 'total-boolean-zero-mask' if total_reference else 'formal-xprop'
    if total_reference:
        result['reference_totality_sha256'] = run.digest(output / 'reference-totality.json')
        result['reference_totality_checked'] = True
        result['gold_data_completion'] = 'fully driven acyclic Boolean graph; explicitly zero unknown masks'
    if partial_gate:
        result['implementation_xprop_sha256'] = run.digest(implementation_path)
        result['implementation_input_unknown_masks'] = 'all zero; no input/reset assumptions'
        result['definedness_obligation'] = 'implementation_unknown & ~reference_unknown == 0 for every compared bit'
    run.save(output / "result.json", result)
    return result


def prove(config, directory, yosys, eqy, abc, seconds=10800, memory_gib=8, limits=None):
    """Prepare from the actual RTL/netlist and return a complete proof or unverified."""
    import lec
    import resource

    directory = Path(directory).resolve()
    directory.mkdir(parents=True, exist_ok=True)
    yosys, eqy, abc = map(str, (yosys, eqy, abc))
    limits = limits or Limits()
    partial_gate = config.get('gate_refinement') is True
    start = time.monotonic()
    steps, certificate = [], {}
    sources = [*map(Path, config["design"]["files"]), Path(config["netlist"]), Path(config["library"])]
    inputs, tools = {}, {}

    def remaining():
        left = seconds - (time.monotonic() - start)
        if left <= 0:
            raise ValueError("transition proof deadline reached")
        return left

    def finish(status, proven=False, message=None):
        peaks = [step.get("peak_rss_kib") for step in steps if step.get("peak_rss_kib") is not None]
        result = {"method": DUAL_METHOD if partial_gate else METHOD, "status": status, "verdict": "proven" if proven else "unverified",
                  "wall_s": time.monotonic() - start,
                  "peak_rss_kib": max([resource.getrusage(resource.RUSAGE_SELF).ru_maxrss, *peaks]),
                  "peak_rss_scope": "process-lifetime upper bound, including adapter and children",
                  "partitions": certificate.get("outputs"), "steps": steps,
                  "source_sha256": inputs, "certificate": certificate,
                  "tools": tools,
                  "scope": {"reference": ("mapped reference netlist with Liberty models" if config.get('reference_gate_netlist') else "input RTL"), "outputs": "all original ports and state-transition obligations",
                            "relation": "inductive event/state refinement with full gate-initial-state coverage",
                            "case_semantics": "procedural priority; no full_case/parallel_case assumptions",
                            "clocks": "clock polarity, latch enables and asynchronous resets checked explicitly",
                            "reset_assumptions": "none", "wire_model": "none"},
                  "limits": {"wall_s": seconds, "memory_gib": memory_gib,
                             "cells": limits.max_cells, "state_bits": limits.max_state,
                             "bits": limits.max_bits, "connections": limits.max_connections}}
        if message:
            result["diagnostic"] = {"code": "BENCH_TRANSITION_UNVERIFIED", "message": message}
        run.save(directory / "result.json", result)
        return result

    try:
        if partial_gate and not config.get('gate_rtl'):
            raise ValueError('partial target refinement requires an RTL intermediate')
        inputs.update({str(path.resolve()): run.digest(path) for path in sources})
        for name, binary, options in (("yosys", yosys, ["-V"]),
                                      ("eqy", eqy, ["--version"]),
                                      ("abc", abc, ["-c", "version"])):
            tools[name] = {"path": str(Path(binary).resolve()), "sha256": run.digest(binary),
                           "version": subprocess.check_output([binary, *options], text=True, timeout=10).strip()}
        model_dir = directory / "models"
        model_dir.mkdir()
        # This is model preparation only. Its unverified status is never counted
        # as a proof; every later coverage/control/data check must pass.
        prepared = lec.prove(config, model_dir, yosys, eqy, abc, seconds=remaining(),
                             memory_gib=memory_gib, multiclock=True, partition_clocks=True,
                             model_only=True, native_storage=True, compact_library_wires=True,
                             max_output_gib=32)
        steps.extend(prepared.get("steps", []))
        if prepared["status"] != "prepared":
            return finish("unverified", message="RTL/netlist model preparation failed")
        case_policy = prepared.get("case_normalization", {})
        if (case_policy.get("policy") != "procedural-priority-no-case-promises"
                or case_policy.get("source_semantics_complete") is not True):
            raise ValueError("reference case semantics were not normalized")
        if any(run.digest(path) != inputs[str(path.resolve())] for path in sources):
            raise ValueError("source changed during proof")
        transition_dir = directory / "transition"
        prepared_state = run.execute(
            [sys.executable, str(Path(__file__).resolve()),
             '--prepare-mapped-models' if config.get('reference_gate_netlist') else '--prepare-models',
             str(model_dir), str(transition_dir), config['design']['top'], yosys,
             str(remaining()), str(memory_gib), json.dumps(limits.__dict__)],
            directory, 'prepare-models', seconds=remaining(), memory_gib=memory_gib,
            max_file_bytes=limits.max_json_bytes)
        steps.append(prepared_state)
        if prepared_state['status'] != 'ok':
            error_path = directory / 'prepare-models-error.json'
            if error_path.exists():
                raise ValueError(run.read_json(error_path)['message'])
            raise ValueError('isolated transition model preparation failed')
        plan = run.read_json(transition_dir / 'state-relation.json')
        steps.append(run.read_json(transition_dir / "export-result.json"))
        steps.append(run.read_json(transition_dir / "alias-metadata-result.json"))
        compared = compare(transition_dir, directory / "cec", config["design"]["top"],
                           yosys, abc, remaining(), memory_gib, limits, partial_gate=partial_gate)
        steps.extend(compared["steps"])
        certificate = {key: value for key, value in plan.items() if key not in ("groups", "forgotten_state")}
        certificate.update({key: value for key, value in compared.items() if key != "steps"})
        certificate["state_relation_sha256"] = run.digest(transition_dir / "state-relation.json")
        certificate["reference_case_semantics"] = case_policy
        certificate["reference_case_normalization_sha256"] = run.digest(model_dir / "case-normalization.json")
        if prepared.get('library_models') is not None:
            certificate['library_models'] = prepared['library_models']
            certificate['library_models_manifest_sha256'] = run.digest(model_dir / 'dual-flops.json')
        certificate["all_state_mapped_or_universally_quantified"] = True
        certificate["initial_relation_covers_all_gate_initial_states"] = True
        certificate['state_name_policy'] = ('exclude generated gate-instance aliases; retain every original obligation'
                                            if config.get('reference_gate_netlist') else 'original RTL state aliases')
        if any(run.digest(path) != inputs[str(path.resolve())] for path in sources):
            raise ValueError("source changed during proof")
        return finish("ok" if compared["equivalent"] else "unverified", compared["equivalent"],
                      None if compared["equivalent"] else "external CEC did not prove every transition/output bit")
    except (ValueError, KeyError, TypeError, OSError, MemoryError, subprocess.SubprocessError) as error:
        return finish("unverified", message=str(error))


if __name__ == '__main__':
    if len(sys.argv) == 9 and sys.argv[1] in ('--prepare-models', '--prepare-models-named', '--prepare-mapped-models'):
        try:
            prepare(Path(sys.argv[2]), Path(sys.argv[3]), sys.argv[4], sys.argv[5],
                    float(sys.argv[6]), int(sys.argv[7]), Limits(**json.loads(sys.argv[8])),
                    keep_names=sys.argv[1] == '--prepare-models-named',
                    mapped_reference=sys.argv[1] == '--prepare-mapped-models')
        except (ValueError, KeyError, TypeError, OSError, MemoryError, subprocess.SubprocessError) as error:
            run.save(Path(sys.argv[3]).parent / 'prepare-models-error.json',
                     {'code': 'BENCH_TRANSITION_PREPARATION', 'message': str(error)})
            raise SystemExit(2)
    elif len(sys.argv) == 6 and sys.argv[1] == '--alias-metadata':
        limits = Limits(**json.loads(sys.argv[5]))
        model = load(Path(sys.argv[2]), sys.argv[4], limits)
        run.save(Path(sys.argv[3]), proposal_aliases(model, limits))
    else:
        raise SystemExit('transition.py is imported by lec.py; only the bounded alias worker runs directly')
