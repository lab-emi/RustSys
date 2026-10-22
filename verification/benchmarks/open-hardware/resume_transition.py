#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Resume only the external CEC stage of a completed transition preparation.

Memory estimate: two bounded AIGs and their port lists, the saved certificate
and at most 200,000 state entries: <512 MiB. One external CEC worker is expected
below 1 GiB for this corpus. Use an outer 4 GiB cgroup and a wall-clock guard.
Copying is streamed, capped at 8 GiB/50,000 files, and rejects symbolic links.
No unfinished preparation, missing guard or changed source/model is accepted.
"""
import argparse
import copy
import json
from pathlib import Path
import shutil
import sys
import time

import run
import transition


PREPARATION_FLAGS = (
    'initial_relation_covers_all_gate_initial_states',
    'all_state_mapped_or_universally_quantified', 'clock_reset_cones_defined',
    'implementation_function_total', 'port_order_verified',
    'projection_graphs_unchanged', 'reference_unknowns_encoded', 'refinement_masks_checked',
)


def check_preparation(directory, proof, abc):
    if proof.get('method') != transition.METHOD:
        raise ValueError('only the complete current transition preparation can be resumed')
    cert = proof.get('certificate', {})
    if any(cert.get(flag) is not True for flag in PREPARATION_FLAGS):
        raise ValueError('a required preparation guard is missing')
    if cert.get('cec_partitioning') is not False or cert.get('cec_matching') != 'verified positional':
        raise ValueError('unverified AIG interface or partition policy')
    if run.digest(abc) != proof['tools']['abc']['sha256']:
        raise ValueError('resume must use the pinned original ABC executable')
    sources = proof.get('source_sha256', {})
    if not sources:
        raise ValueError('missing original source hashes')
    for name, digest in sources.items():
        if run.digest(Path(name)) != digest:
            raise ValueError('original RTL, mapped netlist or library changed')
    files = {'transition/state-relation.json': 'state_relation_sha256',
             'cec/reference-xprop.il': 'reference_xprop_sha256',
             'cec/gold.aig': 'gold_aiger_sha256', 'cec/gate.aig': 'gate_aiger_sha256'}
    for name, field in files.items():
        if run.digest(directory / name) != cert.get(field):
            raise ValueError('prepared model changed: ' + name)
    policy = cert.get('reference_case_semantics', {})
    if (policy.get('policy') != 'procedural-priority-no-case-promises'
            or policy.get('source_semantics_complete') is not True):
        raise ValueError('missing reference case semantics')
    if run.digest(directory / 'models/case-normalization.json') != cert.get('reference_case_normalization_sha256'):
        raise ValueError('reference case normalization changed')
    if run.read_json(directory / 'models/case-normalization.json') != policy:
        raise ValueError('reference case normalization differs from certificate')
    for name, field in [('reference-case-promises.il', 'source_sha256'),
                        ('reference-priority.il', 'normalized_sha256')]:
        if run.digest(directory / 'models' / name) != policy.get(field):
            raise ValueError('reference case model changed: ' + name)
    for source in policy.get('verilog_sources', []):
        if (run.digest(source['source']) != source['source_sha256']
                or run.digest(source['normalized']) != source['normalized_sha256']):
            raise ValueError('normalized reference source changed')
    expected = run.read_json(directory / 'cec/aig-interface.json')
    images = [transition.aig_interface(directory / f'cec/{side}.aig', transition.Limits())
              for side in ('gold', 'gate')]
    for image in images:
        if any(image[kind] != expected[kind] for kind in ('inputs', 'outputs')):
            raise ValueError('AIG port order differs from preparation')
        if len(image['inputs']) != cert['inputs'] or len(image['outputs']) != cert['outputs']:
            raise ValueError('AIG port counts differ from preparation')
    if cert['outputs'] <= 0 or proof.get('partitions') != cert['outputs']:
        raise ValueError('missing transition/output obligations')


def copy_preparation(source, destination):
    if destination.exists() or destination.is_relative_to(source):
        raise ValueError('resume needs a fresh directory outside the original preparation')
    files, size = 0, 0
    for path in source.rglob('*'):
        if path.is_symlink():
            raise ValueError('symlinks are not accepted in a proof preparation')
        if path.is_file():
            files += 1
            size += path.stat().st_size
            if files > 50000 or size > 8 * 1024**3:
                raise ValueError('proof preparation copy budget reached')
    # Do not hard-link: replacing logs or the final result must not modify the
    # previous attempt, including a previous unverified verdict.
    shutil.copytree(source, destination)


def resume(args):
    start = time.monotonic()
    before = run.read_json(args.prepared / 'result.json')
    before_hash = run.digest(args.prepared / 'result.json')
    check_preparation(args.prepared, before, args.abc)
    copy_preparation(args.prepared, args.output)
    check_preparation(args.output, before, args.abc)
    run.save(args.output / 'preparation-result.json', before)
    run.save(args.output / 'resume-command.json', sys.argv)
    output = args.output / 'cec'
    budget = args.timeout - (time.monotonic() - start)
    if budget <= 0:
        raise ValueError('resume deadline reached during preparation checks')
    command = transition.cec_command(transition.Limits(cec_engine=args.engine), budget,
                                     output / 'gold.aig', output / 'gate.aig')
    checked = run.execute([str(args.abc), '-c', command], output, 'cec-whole',
                          seconds=budget, memory_gib=4)
    text = (output / 'cec-whole.stdout').read_text()
    passed = checked['status'] == 'ok' and transition.cec_equivalent(text)
    check_preparation(args.output, before, args.abc)
    if run.digest(args.prepared / 'result.json') != before_hash:
        raise ValueError('previous proof changed during resume')
    (output / 'cec.stdout').write_text(text)
    proof = copy.deepcopy(before)
    cert = proof['certificate']
    cert.update({'equivalent': passed, 'all_output_bits_compared': passed,
                 'outputs_proven': cert['outputs'] if passed else 0,
                 'cec_log_sha256': run.digest(output / 'cec.stdout'),
                 'cec_engine': 'ABC &cec' if args.engine == 'gia' else 'ABC cec -n',
                 'whole_attempt': {'proven': passed, 'status': checked['status'],
                                   'log_sha256': run.digest(output / 'cec-whole.stdout')},
                 'explicit_batches': [{'first': 0, 'count': cert['outputs'], 'proven': passed,
                                       'whole_graph': True,
                                       'log_sha256': run.digest(output / 'cec-whole.stdout')}]})
    proof.update({'status': 'ok' if passed else 'unverified',
                  'verdict': 'proven' if passed else 'unverified',
                  'wall_s': before['wall_s'] + time.monotonic() - start,
                  'peak_rss_kib': (max(before['peak_rss_kib'], checked['peak_rss_kib'])
                                   if before.get('peak_rss_kib') is not None
                                   and checked.get('peak_rss_kib') is not None else None),
                  'steps': [*before['steps'], checked],
                  'resume': {'prepared_result_sha256': before_hash,
                             'directory': str(args.prepared),
                             'wall_s': time.monotonic() - start,
                             'timeout_s': args.timeout,
                             'source_and_model_hashes_rechecked': True}})
    proof.pop('diagnostic', None)
    if not passed:
        proof['diagnostic'] = {'code': 'BENCH_TRANSITION_UNVERIFIED',
                               'message': 'resumed external CEC did not establish equivalence'}
    run.save(output / 'result.json', {k: v for k, v in cert.items() if k not in PREPARATION_FLAGS})
    run.save(args.output / 'result.json', proof)
    print(proof['verdict'], 'resume wall', proof['resume']['wall_s'], flush=True)
    return 0 if passed else 2


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prepared', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--abc', type=Path, required=True)
    parser.add_argument('--timeout', type=int, default=1800)
    parser.add_argument('--engine', choices=('gia', 'legacy'), default='gia')
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error('resume timeout must be positive')
    args.prepared, args.output, args.abc = (p.resolve() for p in (args.prepared, args.output, args.abc))
    return resume(args)


if __name__ == '__main__':
    raise SystemExit(main())
