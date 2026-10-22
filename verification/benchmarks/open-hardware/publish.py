#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Attach checked external certificates to exact measured netlists.

Publication does not run a solver or infer success from partial progress.
Memory: one bounded report/evidence file (8 MiB), sixteen result records and
streamed hashes. Arithmetic revalidation can additionally load a bounded
two-million-node snapshot and its source equations (planning ceiling 1.5 GiB).
The archive is capped at 2 GiB uncompressed,
20,000 entries. Larger reproducible intermediate models are listed by hash.
Use a 2 GiB OS memory guard and a 300 second deadline for a complete publication.
"""
import argparse
import copy
import gzip
import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile

import arithmetic_lec
import lec
import report
import run
import transition

MAX_ENTRY = 8 * 1024**2
MAX_ARCHIVE = 2 * 1024**3
MAX_ENTRIES = 20000


def checked(directory):
    directory = Path(directory).resolve()
    proof = run.read_json(directory / 'result.json')
    if not report.complete_external_proof(proof):
        raise ValueError('publication requires a complete external proof: ' + str(directory))
    method = proof['method']
    if method in transition.METHODS or method in ('yosys-combinational-refinement-v1','output-partitioned-combinational-refinement-v1','boolean-mac-context-replacement-v1'):
        arithmetic_lec.external(directory)
    elif method in (arithmetic_lec.METHOD, arithmetic_lec.TRANSITIVE_METHOD):
        certificate = proof['certificate']
        for name, digest in proof['source_sha256'].items():
            if run.digest(name) != digest:
                raise ValueError('composed certificate input changed: ' + name)
        originals = [Path(name) for name in proof['source_sha256']
                     if Path(name).suffix == '.v'
                     and name in certificate['subproofs'][0]['source_sha256']
                     and name not in (certificate.get('ideal'), certificate.get('intermediate'))]
        if method == arithmetic_lec.TRANSITIVE_METHOD:
            originals = list(map(Path,certificate['original_sources']))
        last_sources = certificate['subproofs'][1]['source_sha256']
        library = next(Path(name) for name in last_sources if Path(name).suffix == '.lib')
        if method == arithmetic_lec.METHOD:
            netlist = next(Path(name) for name in last_sources
                           if Path(name).suffix == '.v' and name != certificate['premap'])
            dirs = certificate['directories']
            with tempfile.TemporaryDirectory() as tmp:
                recreated = arithmetic_lec.compose(
                    Path(tmp), dirs['source'], dirs['arithmetic'], certificate['snapshot'],
                    certificate['premap'], certificate['ideal'], dirs['target'],
                    originals, netlist, library)
        else:
            netlist = next(Path(name) for name in last_sources
                           if Path(name).suffix == '.v' and name != certificate['intermediate'])
            with tempfile.TemporaryDirectory() as tmp:
                recreated = arithmetic_lec.compose_transitive(
                    Path(tmp) / 'proof', *certificate['directories'], originals,
                    certificate['intermediate'], netlist, library)
        if recreated != proof:
            raise ValueError('composition certificate could not be reproduced')
    elif method.startswith('eqy-'):
        work = directory / 'eqy-work'
        parts = [line.split()[1] for line in (work / 'partition.list').read_text().splitlines() if line.strip()]
        if len(parts) != proof['partitions'] or not all(
                item['proven'] for item in lec.strategy_results(work, parts).values()):
            raise ValueError('missing fresh SBY PASS evidence')
        if (directory / 'strategy-certificate.json').exists():
            certificate = run.read_json(directory / 'strategy-certificate.json')
            if (not certificate['all_partitions_proven']
                    or run.digest(directory / 'check.eqy') != certificate['config_sha256']
                    or run.digest(work / 'partition.list') != certificate['partition_list_sha256']):
                raise ValueError('EQY configuration or partition list changed')
        elif not ((work / 'PASS').exists()
                  and 'Successfully proved designs equivalent' in (work / 'logfile.txt').read_text()):
            raise ValueError('missing EQY completion evidence')
    else:
        raise ValueError('unsupported external certificate method')
    return proof


def proof_directories(directory, proof):
    pending, seen = [(Path(directory).resolve(), proof)], set()
    while pending:
        path, current = pending.pop()
        if path in seen:
            continue
        seen.add(path)
        if len(seen) > 256:
            raise ValueError('proof dependency count budget reached')
        yield path
        if current['method'] == arithmetic_lec.METHOD:
            dirs = current['certificate']['directories']
            yield Path(dirs['arithmetic'])
            pending.extend(zip(map(Path, (dirs['source'], dirs['target'])),
                               current['certificate']['subproofs']))
        elif current['method'] == arithmetic_lec.TRANSITIVE_METHOD:
            pending.extend(zip(map(Path, current['certificate']['directories']),
                               current['certificate']['subproofs']))


def publish(args):
    root = run.ROOT
    measurements = args.measurements.resolve()
    result = copy.deepcopy(run.read_json(measurements / 'results.json'))
    selections = run.read_json(args.selections)
    if len(result['rows']) > 64 or len(selections) > 128:
        raise ValueError('publication row budget reached')
    result['proof_publication'] = {
        'date': args.date, 'measurement_root': str(measurements),
        'source_policy': 'procedural priority; no full_case/parallel_case assumptions',
        'initial_state': 'arbitrary implementation state with checked correspondence; no reset sequence',
        'unknowns': 'defined RTL bits checked exactly; undefined RTL bits permit Boolean refinement',
    }
    files, omitted, used, summaries = {}, {}, set(), []
    scanned = 0
    for selection in selections:
        key = tuple(selection[name] for name in ('id', 'library', 'tool'))
        if key in used:
            raise ValueError('duplicate selected proof')
        used.add(key)
        did, library, producer = key
        row = next(row for row in result['rows']
                   if row['design'] == selection['design'] and row['library'] == library)
        measured = row[producer]
        samples = measured['samples']
        expected = samples[-1]['netlist_sha256']
        if (measured['status'] != 'ok' or len(samples) != 3
                or any(s['status'] != 'ok' or s['netlist_sha256'] != expected for s in samples)):
            raise ValueError('incomplete or nondeterministic timed measurements')
        sample_dir = measurements / did / library / (producer + '-3')
        if run.digest(sample_dir / 'netlist.v') != expected:
            raise ValueError('measured netlist changed')
        directory = Path(selection['directory']).resolve()
        proof = checked(directory)
        if proof['method'].startswith('eqy-'):
            config = run.read_json(directory / 'input.json')
            netlist = Path(config.get('netlist', Path(config['directory']) / 'netlist.v'))
            lower = directory / 'equiv-lower.ys'
            if str(netlist) not in lower.read_text():
                raise ValueError('EQY model does not read selected netlist')
            sources = [*map(Path, config['design']['files']), Path(config['library'])]
            current = run.read_json(sample_dir / 'config.json')
            current_sources = [*map(Path, current['design']['files']), Path(current['library'])]
            if [run.digest(p) for p in sources] != [run.digest(p) for p in current_sources]:
                raise ValueError('EQY source or library differs from measured inputs')
            if run.digest(netlist) != expected:
                raise ValueError('EQY proof has a different netlist')
        else:
            current = run.read_json(sample_dir / 'config.json')
            for source in [*current['design']['files'], current['library']]:
                if proof['source_sha256'].get(str(Path(source).resolve())) != run.digest(source):
                    raise ValueError('proof source or PDK differs from measurement')
            if expected not in proof['source_sha256'].values():
                raise ValueError('proof has a different measured netlist')
        prefix = '/'.join(('proofs', did, library, producer))
        proof['evidence_directory'] = prefix
        proof['measured_netlist_sha256'] = expected
        measured.setdefault('lec_attempts', []).append(measured['equivalence'])
        measured['equivalence'] = proof
        summaries.append({**selection, 'directory': prefix, 'method': proof['method'],
                          'partitions': proof['partitions'], 'wall_s': proof['wall_s'],
                          'measured_netlist_sha256': expected})
        for index, folder in enumerate(proof_directories(directory, proof)):
            for path in folder.rglob('*'):
                scanned += 1
                if scanned > 200000:
                    raise ValueError('evidence traversal budget reached')
                if path.is_symlink():
                    raise ValueError('symlinks are not accepted in proof evidence')
                if not path.is_file() or '__pycache__' in path.parts:
                    continue
                if path.suffix not in ('.json', '.ys', '.eqy', '.stdout', '.stderr', '.time',
                                        '.aig', '.aag', '.polys', '.spec', '.proof', '.vcd') \
                        and path.name not in ('status', 'PASS', 'partition.list', 'logfile.txt', 'dual-flops.v'):
                    continue
                name = f'{prefix}/component-{index}/{path.relative_to(folder)}'
                if path.stat().st_size > MAX_ENTRY:
                    omitted[name] = {'bytes': path.stat().st_size, 'sha256': run.digest(path),
                                     'reason': 'reproducible intermediate exceeds evidence entry budget'}
                else:
                    files[name] = path
                if len(files) + len(omitted) > MAX_ENTRIES:
                    raise ValueError('evidence entry budget reached')
        for path in sample_dir.iterdir():
            if path.is_file() and path.suffix in ('.json', '.ys', '.time', '.stdout', '.stderr', '.v'):
                if path.stat().st_size <= MAX_ENTRY:
                    files[f'measurements/{did}/{library}/{producer}/{path.name}'] = path
    for path in run.HERE.iterdir():
        if path.is_file() and path.suffix in ('.py', '.v', '.json'):
            files['harness/' + path.name] = path
    for name in args.evidence:
        path = Path(name).resolve()
        if not path.is_file() or path.stat().st_size > MAX_ENTRY:
            raise ValueError('invalid additional evidence file')
        files['validation/' + str(path.relative_to(root))] = path
    archive_bytes=sum(p.stat().st_size for p in files.values())
    if len(files) > MAX_ENTRIES or archive_bytes > MAX_ARCHIVE:
        raise ValueError(f'evidence archive budget reached: {len(files)} entries, {archive_bytes} bytes')
    args.output.mkdir(parents=True, exist_ok=True)
    manifest = {}
    def normalized(data):
        return data.replace(str(root).encode(), b'$RUSTSYS_ROOT')
    def save(name, value):
        (args.output / name).write_bytes(normalized((json.dumps(value, indent=2, sort_keys=True) + '\n').encode()))
    with (args.output / 'evidence.tar.gz').open('wb') as stream:
        with gzip.GzipFile(fileobj=stream, mode='wb', filename='', mtime=0) as gz:
            with tarfile.open(fileobj=gz, mode='w|') as archive:
                for name, path in sorted(files.items()):
                    data = path.read_bytes()
                    original_digest = hashlib.sha256(data).hexdigest()
                    if original_digest != run.digest(path):
                        raise ValueError('evidence changed while archiving')
                    if path.suffix not in ('.aig',):
                        data = normalized(data)
                    entry = tarfile.TarInfo(name)
                    entry.size, entry.mode, entry.mtime = len(data), 0o644, 0
                    archive.addfile(entry, io.BytesIO(data))
                    manifest[name] = {'original_sha256': original_digest,
                                      'published_sha256': hashlib.sha256(data).hexdigest(), 'bytes': len(data)}
    save('results.json', result)
    save('proofs.json', summaries)
    save('provenance.json', {'files': manifest, 'omitted_models': omitted,
                            'evidence_sha256': run.digest(args.output / 'evidence.tar.gz'),
                            'path_normalization': 'Only local checkout root replaced by $RUSTSYS_ROOT'})
    counts = {tool: sum(report.complete_external_proof(row[tool]['equivalence']) for row in result['rows'])
              for tool in ('rustsys', 'yosys')}
    text = (f'# Arbitrary-state RTL verification, {args.date}\n\n'
            f'Complete external proofs: **RustSys {counts["rustsys"]}/8, Yosys {counts["yosys"]}/8**.\n\n'
            + report.table(result) + '\n\n'
            'Each certificate is checked against the exact netlist from all three timed repetitions. '
            'The proof covers arbitrary implementation initial states under checked state correspondence, '
            'all original outputs, and every retained state transition, clock and reset. No reset sequence '
            'or input assumption is added. Defined RTL bits must match exactly; undefined RTL bits permit '
            'Boolean refinement. Optional full_case/parallel_case promises are removed before parsing for both tools.\n\n'
            'Proofs use complete EQY/SBY results, external ABC CEC, or undef-aware Yosys SAT on every '
            'inductive obligation after all temporal storage is removed. Arithmetic compositions use '
            'AMulet certificates independently checked by Pacheck, or an Arisca signed multiply-add '
            'theorem with exact operand binding and unchanged-context checks, plus both adjoining '
            'whole-design proofs. A timeout, counterexample, memory limit, or partial '
            'proof remains unverified. Native RustSys checker results are recorded separately.\n\n'
            '[Measured data](results.json) · [Selected certificates](proofs.json) · '
            '[Evidence](evidence.tar.gz) · [Hashes and omitted reproducible models](provenance.json) · '
            '[Reproduction](../../README.md)\n')
    (args.output / 'README.md').write_text(text)
    (args.output / 'SHA256SUMS').write_text(''.join(
        f'{run.digest(path)}  {path.name}\n' for path in sorted(args.output.iterdir())
        if path.is_file() and path.name != 'SHA256SUMS'))
    if args.root_readme:
        path = args.root_readme
        path.write_text(report.update_readme_table(path.read_text(), report.table(result)))
    print('Checked and published', counts, 'evidence entries', len(files))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('measurements', 'selections', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--root-readme', type=Path)
    parser.add_argument('--date', required=True)
    parser.add_argument('--evidence', type=Path, action='append', default=[])
    publish(parser.parse_args())
