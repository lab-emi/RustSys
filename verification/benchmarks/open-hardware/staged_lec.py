#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Prove an RTL-to-netlist refinement through independently checked stages.

The checkpoint commands are public Yosys CLI operations, not an implementation
of any reference pass. No checkpoint is trusted: each adjacent pair must pass
the external inductive checker before its certificate can be composed.

One source, at most 19 proof edges, two models resident at a time. Plan for up
to 1.5 million cells x 256 bytes x 2 models plus frontend/solver scratch; Ibex
uses a 4 GiB scope, large cores an 8 GiB scope and the exclusive scale lock.
Keep the caller's physical-memory limit; --memory-gib bounds virtual memory.
Each file is capped at 1 GiB and the whole workspace at 32 GiB/200,000 files.
"""
import argparse
import copy
import fcntl
from pathlib import Path
import time

import arithmetic_lec
import equiv
import lec
import native_comb
import partition_comb
import report
import rtl_semantics
import run
import transition


# This sequence is described by the external executable's `help synth`.
# FSM recoding and memory-read-register merging are deliberately absent, as in
# the measured profile. Every actual input and output remains a proof obligation.
OPERATIONS = (
    'opt_expr', 'opt_clean', 'opt -nodffe -nosdff', 'opt', 'wreduce', 'peepopt',
    'opt_clean', 'alumacc', 'share', 'opt', 'memory -nomap -nordff', 'opt_clean',
    'opt -fast -full', 'memory_map', 'opt -full', 'techmap', 'opt -fast',
)


def prove(config, workspace, synthesis_yosys, yosys, eqy, abc, *, seconds=10800,
          stage_seconds=900, whole_attempt=180, memory_gib=16, unpair=(), through_stage=None):
    workspace = Path(workspace).resolve()
    workspace.mkdir(parents=True, exist_ok=False)
    start = time.monotonic()
    rows = []
    run.save(workspace / 'input.json', config)

    def remaining():
        value = seconds - (time.monotonic() - start)
        if value <= 0:
            raise ValueError('staged proof deadline reached')
        return value

    def execute(command, label, cap):
        result = run.execute(command, workspace, label, seconds=min(cap, remaining()),
                             memory_gib=memory_gib, max_file_bytes=1024**3)
        lec.tree_budget(workspace, max_bytes=32 * 1024**3, max_files=200000)
        if result['status'] != 'ok':
            raise ValueError('staged preparation did not complete: ' + label)
        return result

    try:
        if min(seconds, stage_seconds, memory_gib) <= 0 or whole_attempt < 0 or len(unpair) > 128:
            raise ValueError('invalid staged proof budget')
        if through_stage is not None and not 0 <= through_stage <= len(OPERATIONS):
            raise ValueError('invalid final checkpoint index')
        if len(config['design']['files']) != 1:
            raise ValueError('staged proof requires one self-contained prepared RTL file')
        top = equiv.object_name(config['design']['top'])
        original = Path(config['design']['files'][0]).resolve()
        netlist = Path(config.get('netlist', Path(config['directory']) / 'netlist.v')).resolve()
        library = Path(config['library']).resolve()
        config = copy.deepcopy(config)
        config['library'] = str(library)
        inputs = {str(p): run.digest(p) for p in (original, netlist, library)}
        binaries = {name: {'path': str(Path(path).resolve()), 'sha256': run.digest(path)}
                    for name, path in (('synthesis_yosys', synthesis_yosys), ('yosys', yosys),
                                       ('eqy', eqy), ('abc', abc))}
        run.save(workspace / 'tools.json', binaries)
        policy = rtl_semantics.verilog_priority(original, workspace / 'priority.v')
        run.save(workspace / 'case-source.json', policy)
        capture = [f'read_verilog -sv {equiv.quote(workspace / "priority.v")}',
                   f'hierarchy -check -top {top}', 'proc', 'flatten',
                   f'write_rtlil {equiv.quote(workspace / "s00.il")}']
        for index, operation in enumerate(OPERATIONS, 1):
            capture += [operation, f'write_rtlil {equiv.quote(workspace / f"s{index:02}.il")}']
        script = workspace / 'capture.ys'
        script.write_text('\n'.join(capture) + '\n')
        execute([str(synthesis_yosys), '-Q', '-T', '-s', str(script)], 'capture', 300)
        for index in range(len(OPERATIONS) + 1):
            script = workspace / f'export-{index:02}.ys'
            # Keep word arithmetic intact. Its independently checked native
            # undef-aware SAT encoding avoids unsupported xprop primitives and
            # premature expansion of a multiplier into unrelated gate cones.
            script.write_text(f'read_rtlil {equiv.quote(workspace / f"s{index:02}.il")}\n'
                              f'hierarchy -check -top {top}\nselect {top}\n'
                              f'tee -q rename -enumerate -pattern checkpoint_{index}_% '
                              f'{top}/c:* {top}/w:*$*\n'
                              f'write_verilog -selected -noexpr -norename '
                              f'{equiv.quote(workspace / f"s{index:02}.v")}\n')
            execute([str(synthesis_yosys), '-Q', '-T', '-s', str(script)], f'export-{index:02}', 120)

        previous, chain = original, None
        stop = len(OPERATIONS)+1 if through_stage is None else through_stage
        for index in range(stop + 1):
            intermediate = index <= len(OPERATIONS)
            target = workspace / f's{index:02}.v' if intermediate else netlist
            cfg = copy.deepcopy(config)
            cfg['design']['files'] = [str(previous)]
            cfg['design']['sha256'] = {previous.name: run.digest(previous)}
            cfg.update(netlist=str(target), rtl_icells=index > 0,
                       gate_rtl=intermediate, gate_icells=intermediate,
                       gate_refinement=intermediate)
            limits = transition.Limits(max_batch_outputs=64, whole_attempt_s=whole_attempt)
            directory = workspace / f'proof-{index:02}'
            word_stage = intermediate and index >= 8
            def check(path, hints=()):
                if word_stage:
                    return native_comb.prove(cfg, path, str(yosys), str(eqy), str(abc),
                        seconds=min(stage_seconds, remaining()), memory_gib=memory_gib,
                        constant_exclusions=hints)
                path.mkdir()
                run.save(path / 'input.json', cfg)
                return transition.prove(cfg, path, str(yosys), str(eqy), str(abc),
                    seconds=min(stage_seconds, remaining()), memory_gib=memory_gib,
                    limits=transition.Limits(max_batch_outputs=64, whole_attempt_s=whole_attempt,
                                             constant_exclusions=hints))
            result = check(directory)
            constant_hints = ()
            # A hint only removes an optional constant correspondence. It never
            # removes a port, assumes a reset, or discharges an obligation.
            if not report.complete_external_proof(result) and unpair and (directory / 'transition/gold.json').exists():
                model = transition.load(directory / 'transition/gold.json', top, limits)
                states, aliases, _ = transition.info(model, limits)
                hints = tuple(name for name in unpair if aliases.get(name) in states)
                del model
                if hints:
                    directory = workspace / f'proof-{index:02}-hints'
                    result = check(directory, hints)
                    constant_hints = hints
            retired, selected = set(), set()
            for retry in range(2):
                if not word_stage or report.complete_external_proof(result):
                    break
                failed = directory / 'word' if result.get('method') == partition_comb.METHOD else directory
                retired.update(native_comb.failed_hints(failed))
                selected.update(native_comb.failed_outputs(failed))
                if len(retired)>256 or len(selected)>64:
                    raise ValueError('staged optional-hint/output-partition budget reached')
                directory = workspace / f'proof-{index:02}-refined-{retry}'
                if selected:
                    # Two fresh unconditional proofs; no partial native result
                    # is counted. Exact graph/interface coverage is mandatory.
                    result = partition_comb.prove(cfg, directory, str(yosys), str(eqy), str(abc),
                        tuple(sorted(selected)), seconds=min(stage_seconds, remaining()), memory_gib=memory_gib,
                        internal_exclusions=tuple(sorted(retired)), constant_exclusions=constant_hints)
                else:
                    result = native_comb.prove(cfg, directory, str(yosys), str(eqy), str(abc),
                        seconds=min(stage_seconds, remaining()), memory_gib=memory_gib,
                        internal_exclusions=tuple(sorted(retired)), constant_exclusions=constant_hints,
                        internal_names=None if retired else ())
            rows.append({'stage': index, 'operation': 'proc/flatten' if index == 0 else
                         OPERATIONS[index - 1] if intermediate else 'measured mapped netlist',
                         'directory': str(directory), 'verdict': result['verdict'],
                         'wall_s': result['wall_s']})
            run.save(workspace / 'stages.json', rows)
            print(index, rows[-1]['operation'], result['verdict'], flush=True)
            if not report.complete_external_proof(result):
                raise ValueError('an adjacent stage remains unverified: ' + str(index))
            if chain is None:
                chain = directory
            else:
                destination = workspace / f'chain-{index:02}'
                arithmetic_lec.compose_transitive(destination, chain, directory, [original],
                                                  previous, target, library)
                chain = destination
            previous = target
            if any(run.digest(path) != digest for path, digest in inputs.items()):
                raise ValueError('staged proof inputs changed')
        for binary in binaries.values():
            if run.digest(binary['path']) != binary['sha256']:
                raise ValueError('a staged proof executable changed')
        result = arithmetic_lec.external(chain)
        run.save(workspace / 'result.json', result)
        return result
    except (ValueError, KeyError, TypeError, OSError, MemoryError) as error:
        result = {'method': arithmetic_lec.TRANSITIVE_METHOD, 'status': 'unverified',
                  'verdict': 'unverified', 'wall_s': time.monotonic() - start,
                  'partitions': None, 'completed_stages': sum(row['verdict'] == 'proven' for row in rows),
                  'diagnostic': {'code': 'BENCH_STAGED_LEC', 'message': str(error)},
                  'scope': {'reset_assumptions': 'none'}}
        run.save(workspace / 'result.json', result)
        return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('config', 'workspace', 'synthesis-yosys', 'yosys', 'eqy', 'abc'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--timeout', type=int, default=10800)
    parser.add_argument('--stage-timeout', type=int, default=900)
    parser.add_argument('--whole-attempt', type=int, default=180)
    parser.add_argument('--memory-gib', type=int, default=16)
    parser.add_argument('--unpair-constant', action='append', default=[])
    parser.add_argument('--through-stage',type=int,help='prove only a checkpoint prefix; does not prove the measured netlist')
    args = parser.parse_args()
    if args.workspace.exists():
        parser.error('use a new workspace; completed evidence is never overwritten')
    available = next(int(line.split()[1]) for line in Path('/proc/meminfo').read_text().splitlines()
                     if line.startswith('MemAvailable:'))
    if available < 5 * 1024**2:
        parser.error('less than 5 GiB available; do not start this verification suite')
    # The workspace is new, so directory creation itself excludes concurrent
    # writers. The lock also documents the ownership for external tooling.
    args.workspace.parent.mkdir(parents=True, exist_ok=True)
    with (args.workspace.parent / (args.workspace.name + '.lock')).open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        result = prove(run.read_json(args.config), args.workspace,
                       *[getattr(args, name).resolve() for name in ('synthesis_yosys', 'yosys', 'eqy', 'abc')],
                       seconds=args.timeout, stage_seconds=args.stage_timeout,
                       whole_attempt=args.whole_attempt, memory_gib=args.memory_gib,
                       unpair=args.unpair_constant,through_stage=args.through_stage)
    raise SystemExit(0 if result['verdict'] == 'proven' else 2)
