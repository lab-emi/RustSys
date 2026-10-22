# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Bounded extraction of diagnostic multiplier observations for external proofs.

The snapshot is untrusted evidence: validate topology and cut interfaces before
export. Expected snapshot memory is below 1 GiB (two million bounded nodes); an
individual extracted cone has at most 200,000 nodes. One external worker runs at
a time under the caller's cgroup. AMulet and Pacheck are opaque executables.
"""
from pathlib import Path
import json
import re
import time

import run

MAX_NODES = 2_000_000
MAX_CONE = 200_000
MAX_SITES = 64


def polynomial(text):
    text = text.strip().removesuffix(';')
    if len(text) > 262144:
        raise ValueError('polynomial text budget reached')
    result = {}
    terms = re.findall(r'[+-]?[^+-]+', text)
    if ''.join(terms) != text or len(terms) > 4096:
        raise ValueError('invalid or oversized polynomial')
    for term in terms:
        coefficient = -1 if term.startswith('-') else 1
        factors = term.lstrip('+-').split('*')
        if factors[0].isdigit():
            if len(factors[0]) > 40:
                raise ValueError('polynomial coefficient budget reached')
            coefficient *= int(factors.pop(0))
        if any(not re.fullmatch(r'[a-z][a-z0-9_]*', v) for v in factors):
            raise ValueError('invalid polynomial variable')
        key = tuple(sorted(factors))
        result[key] = result.get(key, 0) + coefficient
    return {key: value for key, value in result.items() if value}


def _add(left, right, scale=1):
    result = dict(left)
    for key, value in right.items():
        result[key] = result.get(key, 0) + scale * value
    return {key: value for key, value in result.items() if value}


def _multiply(left, right):
    result = {}
    for a, x in left.items():
        for b, y in right.items():
            key = tuple(sorted(a+b))
            result[key] = result.get(key, 0) + x*y
    return {key: value for key, value in result.items() if value}


def check_encoding(aag, polys, spec, width, signed):
    """Independently bind the algebraic proof to AND truth and integer product."""
    lines = Path(aag).read_text().splitlines()
    kind, maximum, ni, nl, no, ng = lines[0].split()
    maximum, ni, nl, no, ng = map(int, (maximum, ni, nl, no, ng))
    if kind != 'aag' or nl or ni != 2*width or no != 2*width or maximum != ni+ng:
        raise ValueError('polynomial component interface mismatch')
    if [int(x) for x in lines[1:1+ni]] != list(range(2, 2*ni+1, 2)):
        raise ValueError('polynomial input order mismatch')
    names = {i+1: (f'a{i}' if i < width else f'b{i-width}') for i in range(ni)}
    def literal(value):
        if value < 2:
            return {(): value} if value else {}
        variable = names.get(value//2, f'l{value & ~1}')
        base = {(variable,): 1}
        return _add({(): 1}, base, -1) if value & 1 else base
    expected = [{(): 1 << no}]
    for line in lines[1+ni+no:1+ni+no+ng]:
        lhs, a, b = map(int, line.split())
        expected.append(_add(_multiply(literal(a), literal(b)), literal(lhs), -1))
    for index, line in enumerate(lines[1+ni:1+ni+no]):
        expected.append(_add(literal(int(line)), {(f's{index}',): 1}, -1))
    actual = [polynomial(line.split(' ', 1)[1]) for line in Path(polys).read_text().splitlines()]
    canonical = lambda p: tuple(sorted(p.items()))
    if sorted(map(canonical, actual)) != sorted(map(canonical, expected)):
        raise ValueError('algebraic gate constraints differ from the AIG')
    def word(prefix, bits):
        return {(f'{prefix}{i}',): (-1 if signed and i == bits-1 else 1) * (1 << i) for i in range(bits)}
    target = _add(_multiply(word('a', width), word('b', width)), word('s', no), -1)
    if polynomial(Path(spec).read_text()) != target:
        raise ValueError('algebraic target is not the requested integer multiplication')


def load_snapshot(path):
    path = Path(path)
    if path.stat().st_size > 256 * 1024**2:
        raise ValueError('arithmetic snapshot byte budget reached')
    snapshot = json.loads(path.read_text())
    nodes = snapshot['nodes']
    if len(nodes) > MAX_NODES or len(snapshot['multipliers']) > MAX_SITES:
        raise ValueError('arithmetic snapshot graph budget reached')
    for index, pair in enumerate(nodes):
        if pair is not None and (len(pair) != 2 or any(type(v) is not int or v < 0 or v // 2 >= index for v in pair)):
            raise ValueError('invalid or cyclic arithmetic snapshot')
    references = 0
    for site in snapshot['multipliers']:
        if type(site.get('signed')) is not bool:
            raise ValueError('invalid arithmetic signedness')
        for key in ('a','b','product'):
            values = site[key]
            references += len(values)
            if not 0 < len(values) <= 128 or any(type(v) is not int or not 0 <= v < 2*len(nodes) for v in values):
                raise ValueError('invalid arithmetic observation vector')
    if references > 262144:
        raise ValueError('arithmetic observation reference budget reached')
    return snapshot


def significant(bits, signed):
    n = len(bits)
    while n > 1 and (bits[n - 1] == bits[n - 2] if signed else bits[n - 1] == 0):
        n -= 1
    return bits[:n]


def extract(snapshot, index, output):
    site = snapshot['multipliers'][index]
    a = significant(site['a'], site['signed'])
    b = significant(site['b'], site['signed'])
    if len(a) != len(b) or not 1 <= len(a) <= 32:
        raise ValueError('component currently requires equal significant operand widths <= 32')
    width = len(a)
    cuts = a + b
    fixed = [i for i, v in enumerate(cuts) if v < 2]
    if (any(cuts[i] != 0 or not site['signed'] or i not in (width-1, 2*width-1) for i in fixed)
            or len(fixed) > 1
            or len({v // 2 for v in cuts if v >= 2}) != len(cuts)-len(fixed)):
        raise ValueError('component operands are not independent cut nodes')
    product = site['product']
    if len(product) < 2 * width:
        raise ValueError('truncated component needs a separate completion certificate')
    extension = product[2 * width - 1] if site['signed'] else 0
    if any(v != extension for v in product[2 * width:]):
        raise ValueError('component extension is not structurally verified')
    nodes = snapshot['nodes']
    image = {0: 0}
    for position, literal in enumerate(cuts, 1):
        if literal >= 2:
            image[literal // 2] = 2 * position ^ (literal & 1)
    todo = [v // 2 for v in product[:2 * width]]
    visited = set()
    while todo:
        node = todo.pop()
        if node in image or node in visited:
            continue
        if node >= len(nodes) or nodes[node] is None:
            raise ValueError('component depends on an input outside its operand cuts')
        visited.add(node)
        if len(visited) > MAX_CONE:
            raise ValueError('component cone budget reached')
        todo.extend(v // 2 for v in nodes[node])
    gates = []
    def lit(value):
        return image[value // 2] ^ (value & 1)
    for node in sorted(visited):
        first, second = sorted((lit(v) for v in nodes[node]), reverse=True)
        target = 2 * (len(cuts) + len(gates) + 1)
        gates.append((target, first, second))
        image[node] = target
    outputs = [lit(v) for v in product[:2 * width]]
    with Path(output).open('w') as stream:
        stream.write(f'aag {len(cuts)+len(gates)} {len(cuts)} 0 {len(outputs)} {len(gates)}\n')
        for n in range(1, len(cuts) + 1):
            stream.write(f'{2*n}\n')
        for value in outputs:
            stream.write(f'{value}\n')
        for values in gates:
            stream.write(' '.join(map(str, values)) + '\n')
        for i in range(len(cuts)):
            stream.write(f'i{i} i{i}\n')
        for i in range(len(outputs)):
            stream.write(f'o{i} o{i}\n')
        stream.write('c\nindependent operand cut, little endian A then B\n')
    return {'index': index, 'width': width, 'signed': site['signed'], 'a': a, 'b': b,
            'product': product, 'cut_nodes': cuts, 'nodes': len(gates),
            'source_nodes': sorted(visited),
            'fixed_zero_inputs': fixed,
            'aag_sha256': run.digest(output)}


def _zero_cofactor(source, destination, index):
    lines = Path(source).read_text().splitlines()
    kind, maximum, ni, nl, no, ng = lines[0].split()
    ni, nl, no, ng = map(int, (ni, nl, no, ng))
    if kind != 'aag' or nl or not 0 <= index < ni or ng > MAX_CONE:
        raise ValueError('unsupported cofactor interface')
    def bit(value):
        return value & 1 if value//2 == index+1 else value
    for i in range(1+ni, 1+ni+no):
        lines[i] = str(bit(int(lines[i])))
    for i in range(1+ni+no, 1+ni+no+ng):
        lhs, a, b = map(int, lines[i].split())
        lines[i] = f'{lhs} {bit(a)} {bit(b)}'
    end = 1+ni+no+ng
    symbols, comments = [], []
    for line in lines[end:]:
        if re.match(r'^[io]\d+ ', line):
            symbols.append(line)
        else:
            comments.append(line)
    # Preserve the AIGER numeric symbol indices, independent of serialization
    # order. Some importers use encounter order when assigning HDL port IDs.
    symbols.sort(key=lambda s: (s[0], int(s.split(' ')[0][1:])))
    Path(destination).write_text('\n'.join(lines[:end]+symbols+comments)+'\n')


def _complete_zero_sign(folder, aag, certificate, yosys, abc, budget=lambda value:value):
    """Lift a zero sign-bit product into a general square signed product.

    For B[n-1]=0, C=A*B. Restoring that bit changes the product by
    -A*B[n-1]*2**(n-1). Verify the lift algebraically and its zero-bit
    specialization against the extracted circuit with external CEC.
    """
    width = certificate['width']
    index, = certificate['fixed_zero_inputs']
    fixed_word, other = ('a', 'b') if index < width else ('b', 'a')
    ports = [f'.i{i}({"a" if i < width else "b"}[{i % width}])' for i in range(2*width)]
    ports += [f'.o{i}(c[{i}])' for i in range(2*width)]
    wrapper = folder / 'complete-sign.v'
    # A local ripple network avoids introducing a parallel-prefix subtractor
    # into the algebraic certificate. Its behavior is checked by both external
    # tools; the adapter is not trusted as an arithmetic oracle.
    subtract = [f'wire [{2*width}:0] carry; assign carry[0] = 1\'b1;']
    for i in range(2*width):
        subtract.append(f'assign p[{i}] = c[{i}] ^ ~correction[{i}] ^ carry[{i}];')
        subtract.append(f'assign carry[{i+1}] = (c[{i}] & ~correction[{i}]) | (c[{i}] & carry[{i}]) | (~correction[{i}] & carry[{i}]);')
    wrapper.write_text(
        f'module complete(input [{width-1}:0] a,b,output [{2*width-1}:0] p);\n'
        f'wire [{2*width-1}:0] c; raw original (' + ','.join(ports) + ');\n'
        f'wire [{2*width-1}:0] extended = {{{{{width}{{{other}[{width-1}]}}}},{other}}};\n'
        f'wire [{2*width-1}:0] correction = (extended << {width-1}) & {{{2*width}{{{fixed_word}[{width-1}]}}}};\n'
        + '\n'.join(subtract) + '\nendmodule\n')
    lifted = folder / 'complete-sign.aag'
    script = folder / 'complete-sign.ys'
    script.write_text(f'read_aiger -module_name raw {run.quote(aag)}\nread_verilog {run.quote(wrapper)}\n'
                      'hierarchy -check -top complete\nflatten\ntechmap\nopt -fast\naigmap\nopt_clean\n'
                      f'write_aiger -ascii -symbols {run.quote(lifted)}\n')
    prepared = run.execute([str(yosys), '-s', str(script)], folder, 'complete-sign', seconds=budget(60), memory_gib=2)
    if prepared['status'] != 'ok':
        raise ValueError('sign completion frontend failed')
    # The tool's physical bit order is part of the proof, never inferred.
    symbols = sorted((line for line in lifted.read_text().splitlines() if re.match(r'^i\d+ ', line)), key=lambda s: int(s.split(' ')[0][1:]))
    expected = [f'i{i} {"a" if i < width else "b"}[{i % width}]' for i in range(2*width)]
    if symbols != expected:
        raise ValueError('sign completion input order differs')
    outputs = sorted((line for line in lifted.read_text().splitlines() if re.match(r'^o\d+ ', line)), key=lambda s: int(s.split(' ')[0][1:]))
    if outputs != [f'o{i} p[{i}]' for i in range(2*width)]:
        raise ValueError('sign completion output order differs')
    cofactor = folder / 'complete-sign-zero.aag'
    _zero_cofactor(lifted, cofactor, index)
    # This ABC build accepts binary AIGER; convert without reordering ports.
    abc_original, abc_cofactor = folder/'original.aig', folder/'zero-cofactor.aig'
    convert = folder / 'cofactor-binary.ys'
    convert.write_text(f'read_aiger -module_name raw {run.quote(aag)}\n'
                       f'write_aiger -no-sort -symbols {run.quote(abc_original)}\ndesign -reset\n'
                       f'read_aiger -module_name raw {run.quote(cofactor)}\n'
                       f'write_aiger -no-sort -symbols {run.quote(abc_cofactor)}\n')
    converted = run.execute([str(yosys), '-s', str(convert)], folder, 'cofactor-binary', seconds=budget(30), memory_gib=2)
    if converted['status'] != 'ok':
        raise ValueError('cofactor binary conversion failed')
    import transition
    before = transition.aig_interface(abc_original, transition.Limits())
    after = transition.aig_interface(abc_cofactor, transition.Limits())
    if before['inputs'] != [f'i{i}' for i in range(2*width)] or before['outputs'] != [f'o{i}' for i in range(2*width)]:
        raise ValueError('original arithmetic cofactor port order changed')
    if after['inputs'] != [f'{"a" if i < width else "b"}[{i % width}]' for i in range(2*width)] or after['outputs'] != [f'p[{i}]' for i in range(2*width)]:
        raise ValueError('lifted arithmetic cofactor port order changed')
    command = f'cec -n -T 60 -C 100000 -I 10000000 {run.quote(abc_original)} {run.quote(abc_cofactor)};'
    checked = run.execute([str(abc), '-c', command], folder, 'complete-sign-check', seconds=budget(65), memory_gib=2)
    if checked['status'] != 'ok' or not transition.cec_equivalent((folder/'complete-sign-check.stdout').read_text()):
        raise ValueError('sign completion did not preserve the original cofactor')
    certificate['sign_completion'] = {'lifted_sha256': run.digest(lifted),
        'zero_cofactor_sha256': run.digest(cofactor), 'cec': checked,
        'binary_original_sha256': run.digest(abc_original),
        'binary_cofactor_sha256': run.digest(abc_cofactor),
        'conversion_script_sha256': run.digest(convert),
        'cec_log_sha256': run.digest(folder/'complete-sign-check.stdout')}
    return lifted


def certify(snapshot_path, directory, amulet, pacheck, yosys=None, abc=None, deadline=None):
    snapshot = load_snapshot(snapshot_path)
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=False)
    certificates = []
    def budget(value):
        remaining = value if deadline is None else min(value, deadline-time.monotonic())
        if remaining <= 0:
            raise ValueError('arithmetic certificate deadline reached')
        return remaining
    for index in range(len(snapshot['multipliers'])):
        folder = directory / str(index)
        folder.mkdir()
        aag = folder / 'component.aag'
        certificate = extract(snapshot, index, aag)
        proof_aag = aag
        if certificate['fixed_zero_inputs']:
            if yosys is None or abc is None:
                raise ValueError('zero sign-bit completion requires external Yosys and ABC')
            proof_aag = _complete_zero_sign(folder, aag, certificate, yosys, abc, budget)
        polys, proof, spec = (folder / name for name in ('component.polys', 'component.proof', 'component.spec'))
        command = [str(amulet), '-certify', str(proof_aag), str(polys), str(proof), str(spec), '-p2']
        if certificate['signed']:
            command.append('-signed')
        result = run.execute(command, folder, 'amulet', seconds=budget(120), memory_gib=2,
                             max_file_bytes=256 * 1024**2)
        text = (folder / 'amulet.stdout').read_text()
        if result.get('exit_code') != 1 or not re.search(r'^\[amulet2\] CORRECT MULTIPLIER\s*$', text, re.M):
            raise ValueError('AMulet did not certify the extracted multiplier')
        check_encoding(proof_aag, polys, spec, certificate['width'], certificate['signed'])
        # This release prefixes its LPAC stream with a human-readable duplicate
        # of the input polynomials. Validate that duplicate, then pass every
        # numbered inference unchanged to the independent checker.
        if proof.stat().st_size > 256 * 1024**2:
            raise ValueError('polynomial proof byte budget reached')
        lines = proof.read_text().splitlines(keepends=True)
        first = next((i for i, line in enumerate(lines) if re.match(r'^\d+\s', line)), None)
        if first is None:
            raise ValueError('polynomial proof has no inferences')
        originals = {line.split(' ', 1)[1].strip() for line in polys.read_text().splitlines()}
        at = 0
        while at < first:
            if not re.fullmatch(r'\[amulet2\] (?:gate|output) [a-zA-Z0-9_]+:\n', lines[at]):
                raise ValueError('unexpected proof preamble')
            if at + 2 >= first or lines[at + 1].strip() not in originals or lines[at + 2].strip():
                raise ValueError('proof preamble differs from original constraints')
            at += 3
        checked_proof = folder / 'checked.proof'
        checked_proof.write_text(''.join(lines[first:]))
        checked = run.execute([str(pacheck), str(polys), str(checked_proof), str(spec)], folder,
                              'pacheck', seconds=budget(120), memory_gib=2,
                              max_file_bytes=256 * 1024**2)
        if checked['status'] != 'ok' or not re.search(r'^\[pck2\] c TARGET CHECKED\s*$', (folder / 'pacheck.stdout').read_text(), re.M):
            raise ValueError('independent polynomial proof check failed')
        certificate.update({'amulet': result, 'pacheck': checked,
                            'gate_encoding_checked': True, 'integer_product_target_checked': True,
                            'proof_sha256': run.digest(proof), 'spec_sha256': run.digest(spec),
                            'checked_proof_sha256': run.digest(checked_proof),
                            'amulet_log_sha256': run.digest(folder/'amulet.stdout'),
                            'pacheck_log_sha256': run.digest(folder/'pacheck.stdout'),
                            'polys_sha256': run.digest(polys)})
        run.save(folder / 'certificate.json', certificate)
        certificates.append(certificate)
    result = {'snapshot_sha256': run.digest(snapshot_path), 'components': certificates,
              'complete': bool(certificates), 'amulet_sha256': run.digest(amulet),
              'pacheck_sha256': run.digest(pacheck)}
    run.save(directory / 'certificate.json', result)
    return result


def idealize(snapshot_path, source, destination, certificates):
    """Replace only certified result-node drivers by ideal word operators."""
    snapshot = load_snapshot(snapshot_path)
    if run.digest(snapshot_path) != certificates['snapshot_sha256'] or not certificates['complete']:
        raise ValueError('arithmetic snapshot differs from its certificates')
    nodes = snapshot['nodes']
    drivers, declarations, replacements = {}, [], []
    def expr(value):
        return f"1'b{value}" if value < 2 else ('~' if value & 1 else '') + f'_diag_n{value//2}'
    if Path(source).stat().st_size > 256 * 1024**2:
        raise ValueError('arithmetic source byte budget reached')
    source_text = Path(source).read_text()
    if '`' in source_text:
        raise ValueError('arithmetic source must be a direct diagnostic export, without macros')
    equation_pattern = re.compile(r'^assign _diag_n(\d+) = [^\n]+;$', re.M)
    equations = {}
    for match in equation_pattern.finditer(source_text):
        node = int(match[1])
        if node in equations or len(equations) > MAX_NODES:
            raise ValueError('arithmetic source equation count or identity mismatch')
        equations[node] = match[0]
    for item in certificates['components']:
        index, width = item['index'], item['width']
        site = snapshot['multipliers'][index]
        for node in item['source_nodes']:
            pair = nodes[node]
            expected = f'assign _diag_n{node} = {expr(pair[0])} & {expr(pair[1])};'
            if equations.get(node) != expected:
                raise ValueError('arithmetic snapshot is not bound to the source node equation')
        dependencies, todo = set(), [v // 2 for v in item['cut_nodes']]
        while todo:
            node = todo.pop()
            if node in dependencies:
                continue
            dependencies.add(node)
            if len(dependencies) > MAX_CONE:
                raise ValueError('operand fan-in budget reached')
            if nodes[node] is not None:
                todo.extend(v // 2 for v in nodes[node])
        left = '{' + ','.join(expr(v) for v in reversed(item['a'])) + '}'
        right = '{' + ','.join(expr(v) for v in reversed(item['b'])) + '}'
        if site['signed']:
            left, right = f'$signed({left})', f'$signed({right})'
        declarations.append(f'wire [{2*width-1}:0] __ideal_mul_{index} = {left} * {right};')
        for bit, literal in enumerate(site['product'][:2 * width]):
            node = literal // 2
            if node == 0 or nodes[node] is None or node in dependencies or node in drivers:
                continue
            drivers[node] = ('~' if literal & 1 else '') + f'__ideal_mul_{index}[{bit}]'
            replacements.append({'node': node, 'site': index, 'bit': bit, 'complement': bool(literal & 1)})
    text = source_text
    header = text.find('\n') + 1
    text = text[:header] + '\n'.join(declarations) + '\n' + text[header:]
    changed = set()
    def replacement(match):
        node = int(match[1])
        if node not in drivers:
            return match[0]
        changed.add(node)
        return f'assign _diag_n{node} = {drivers[node]};'
    text = equation_pattern.sub(replacement, text)
    if changed != drivers.keys():
        raise ValueError('missing or repeated arithmetic result driver')
    Path(destination).write_text(text)
    return {'source_sha256': run.digest(source), 'ideal_sha256': run.digest(destination),
            'snapshot_sha256': certificates['snapshot_sha256'], 'replacements': replacements}
