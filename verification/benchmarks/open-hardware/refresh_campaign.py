#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Validate a new 16-core producer against retained, pinned reference campaigns.

Parent: <=15 rows, <=64 MiB JSON, <512 MiB RSS. Streamed hashes only.
Existing bounded workers: small LEC 4 GiB, full-core model 24 GiB, STA 32 GiB.
Full-core work is serialized by scale.lock. Reuse requires identical netlist,
RTL, library, top and complete proof; changed netlists require fresh LEC/STA.
No reset/input assumptions are added. Run parent with a 40 GiB AS/wall guard.
"""
from pathlib import Path
import argparse
import copy
import fcntl

import additional_verify
import evidence_reuse
import functional_models
import run
import scaling_report
import scaling_verify
import sta_benchmark

LIBRARIES = ('sky130hd_tt', 'gf180mcu7t_tt', 'ez130_8t_tt')


def matrix(data):
    rows = {(r['id'], r['library']): r for r in data['rows']}
    required = {(did, lib) for did in scaling_report.ORDER for lib in LIBRARIES}
    if (len(data['rows']) != 15 or set(rows) != required
            or data['limits']['repetitions'] != 3 or data['limits']['warmups'] != 1):
        raise ValueError('expected complete five-design/three-PDK measurements')
    for row in rows.values():
        value = row.get('rustsys', {})
        if (row['allocated_physical_cores'] != 16 or len(set(row['cpu_affinity'])) != 16
                or value.get('status') != 'ok' or len(value.get('samples', [])) != 3
                or any(s['status'] != 'ok' or s['netlist_sha256'] != value['netlist_sha256']
                       for s in value['samples'])):
            raise ValueError('incomplete/nondeterministic 16-core producer measurement')
    return rows


def reference_config(workspace, row, tool):
    # Joined historical datasets retain the original measured configuration.
    folder = workspace / row['id'] / row['library'] / '16c' / (tool + '-3')
    config = run.read_json(folder / 'config.json')
    original = Path(config['directory'])
    if original.parts[-4:] != folder.parts[-4:]:
        raise ValueError('retained reference location changed its design/PDK/core/tool')
    config = additional_verify.checked_config(original.parents[3], row, tool)
    if run.digest(folder / 'netlist.v') != row[tool]['netlist_sha256']:
        raise ValueError('retained reference netlist changed')
    return config


def prove(cfg, dest, sample, checker):
    receipt = dest / 'result.json'
    if receipt.exists():
        proof = run.read_json(receipt)
    else:
        with (run.ROOT / '.work/scale.lock').open('a') as lock:
            if cfg['design']['id'] == 'openc910':
                fcntl.flock(lock, fcntl.LOCK_EX)
            proof = scaling_verify.native_case(cfg, dest, sample, checker)
    if not scaling_verify.complete(proof) and cfg['design']['id'] in ('ibex', 'vexriscv-verilog'):
        logs = (dest / 'abc-cec.stdout', dest / 'export/abc-cec.stdout')
        if any(any(s in p.read_text() for s in ('NOT EQUIVALENT', 'Verification failed'))
               for p in logs if p.exists()):
            raise ValueError('native counterexample requires investigation')
        proof = scaling_verify.observed_case(cfg, dest / 'composed', event_checker=checker)
    if not scaling_verify.complete(proof):
        raise ValueError('incomplete arbitrary-state LEC: ' + str(dest))
    return proof


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('workspace', 'directory', 'event-checker', 'sta'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--baseline', nargs=2, type=Path, action='append', required=True,
                        metavar=('MEASUREMENTS_DIRECTORY', 'VALIDATION_DIRECTORY'))
    parser.add_argument('--reuse', nargs=2, type=Path, action='append', default=[], metavar=('PROOFS_JSON', 'TIMING_JSON'),
                        help='additional complete exact-byte receipts; every binding is rechecked')
    parser.add_argument('--functional-models', type=Path, required=True)
    parser.add_argument('--phase', choices=('proofs', 'timing', 'all'), default='all')
    args = parser.parse_args()
    args.event_checker = args.event_checker.resolve()
    args.sta = args.sta.resolve()
    args.functional_models = args.functional_models.resolve()
    if not 1 <= len(args.baseline) <= 4 or len(args.baseline) + len(args.reuse) > 8:
        parser.error('one to four baselines and at most eight evidence sources required')
    ws = args.workspace.resolve(); out = args.directory.resolve()
    out.mkdir(parents=True, exist_ok=True)
    if (ws / 'results.json').stat().st_size > 64 * 1024**2: raise ValueError('measurement byte budget')
    data = run.read_json(ws / 'results.json'); rows = matrix(data)
    additional_verify.check_hashes({**data['binary_sha256'], **data['input_sha256']})
    baseline = {}; source_pins = {}; proof_paths = []; timing_paths = []
    for measurements, validation in args.baseline:
        measurements = measurements.resolve(); validation = validation.resolve()
        if sum(p.stat().st_size for p in (measurements / 'results.json', validation / 'proofs.json', validation / 'timing.json')) > 64 * 1024**2:
            raise ValueError('baseline byte budget')
        original = run.read_json(measurements / 'results.json')
        for path in (measurements / 'results.json', validation / 'proofs.json', validation / 'timing.json'):
            source_pins[str(path)] = run.digest(path)
        proof_paths.append(validation / 'proofs.json'); timing_paths.append(validation / 'timing.json')
        for row in original['rows']:
            if row['allocated_physical_cores'] != 16: continue
            key = row['id'], row['library']
            if key in baseline: raise ValueError('duplicate reference row')
            baseline[key] = (measurements, row)
    if set(baseline) != set(rows): raise ValueError('reference coverage differs')
    for proof, timing_path in args.reuse:
        for path in (proof.resolve(), timing_path.resolve()): source_pins[str(path)] = run.digest(path)
        proof_paths.append(proof.resolve()); timing_paths.append(timing_path.resolve())
    pi = evidence_reuse.load(proof_paths, 'proof'); ti = evidence_reuse.load(timing_paths, 'timing')
    proofs = {'schema': 1, 'scope': {'initial_states': 'arbitrary', 'reset_assumptions': 'none',
              'input_assumptions': 'none'}, 'rows': []}
    timing = {'schema': 1, 'rows': []}
    quality = {'schema': 1, 'rows': []}
    combined = copy.deepcopy(data); combined['rows'] = []
    combined['source_campaigns'] = source_pins
    combined['scope']['reference_measurements'] = 'retained original pinned Yosys runs; not re-timed'
    for did in scaling_report.ORDER:
        for lib in LIBRARIES:
            key = did, lib; row = rows[key]; old_ws, old = baseline[key]
            if old['cpu_affinity'] != row['cpu_affinity']:
                raise ValueError('reference CPU allocation differs')
            joined = copy.deepcopy(row); joined['yosys'] = copy.deepcopy(old['yosys'])
            joined['reference_measurements'] = str(old_ws / 'results.json')
            combined['rows'].append(joined)
            pr = {'id': did, 'library': lib}; tr = dict(pr)
            for tool in ('rustsys', 'yosys'):
                sample_row = row if tool == 'rustsys' else old
                config = (additional_verify.checked_config(ws, sample_row, tool) if tool == 'rustsys'
                          else reference_config(old_ws, sample_row, tool))
                if tool == 'rustsys': current_config = config
                else:
                    if ({k: v for k, v in config['design'].items() if k != 'files'} !=
                            {k: v for k, v in current_config['design'].items() if k != 'files'}
                            or [run.digest(p) for p in config['design']['files']] !=
                               [run.digest(p) for p in current_config['design']['files']]
                            or run.digest(config['library']) != run.digest(current_config['library'])):
                        raise ValueError('retained reference uses different RTL/top/library')
                netlist = Path(config['directory']) / 'netlist.v'
                cfg = copy.deepcopy(config)
                supplement = functional_models.prepare(cfg['library'], args.functional_models, cfg['yosys'])
                if supplement: cfg['library'] = supplement['functional_library']
                if did == 'openc910':
                    cfg.update(export_discovery_work=64000000000, cec_solver='new')
                if args.phase in ('proofs', 'all'):
                    proof = evidence_reuse.proof(pi, (*key, tool), cfg, netlist, scaling_verify.complete)
                    if proof is None:
                        if tool == 'yosys': raise ValueError('pinned reference evidence does not bind')
                        print('Fresh LEC:', did, lib, tool, flush=True)
                        proof = prove(cfg, out / did / lib / tool / 'native', row[tool]['samples'][-1], args.event_checker)
                        proof = functional_models.bind(proof, supplement)
                    pr[tool] = proof
                    additional_verify.check_hashes(proof['source_sha256'])
                    print('Proven:', did, lib, tool, proof.get('reuse_binding', {}).get('status', 'fresh'), flush=True)
                if args.phase in ('timing', 'all'):
                    result = evidence_reuse.timing(ti, (*key, tool), netlist, config['library'], args.sta, scaling_verify.CLOCK[did])
                    if result is None:
                        dest = out / did / lib / tool / 'sta'
                        result = run.read_json(dest / 'result.json') if (dest / 'result.json').exists() else sta_benchmark.measure(
                            netlist, config['library'], config['design']['top'], scaling_verify.CLOCK[did], args.sta,
                            dest, memory_gib=32 if did == 'openc910' else 4, seconds=7200, cpu=31)
                    if result['status'] != 'ok': raise ValueError('incomplete OpenSTA')
                    additional_verify.check_hashes({**result['sha256'], **result['receipts_sha256']})
                    tr[tool] = result
                    print('Fmax:', did, lib, tool, result['fmax_mhz'], flush=True)
            if args.phase in ('timing', 'all'):
                before = ti[(*key, 'rustsys')][0][2]
                old_area = old['rustsys']['qor']['area_um2']; new_area = row['rustsys']['qor']['area_um2']
                result = {'id': did, 'library': lib, 'old_area_um2': old_area, 'new_area_um2': new_area,
                          'old_fmax_mhz': before['fmax_mhz'], 'new_fmax_mhz': tr['rustsys']['fmax_mhz'],
                          'old_wall_s': old['rustsys']['median_wall_s'], 'new_wall_s': row['rustsys']['median_wall_s'],
                          'identical_netlist': old['rustsys']['netlist_sha256'] == row['rustsys']['netlist_sha256'],
                          'area_nonregressing': new_area <= old_area + 1e-5,
                          'fmax_nonregressing': tr['rustsys']['fmax_mhz'] >= before['fmax_mhz'] - 1e-10}
                quality['rows'].append(result); run.save(out / 'quality.json', quality)
                if not result['area_nonregressing'] or not result['fmax_nonregressing']:
                    raise ValueError('quality regression: ' + str(result))
            if args.phase in ('proofs', 'all'):
                proofs['rows'].append(pr); run.save(out / 'proofs.json', proofs)
            if args.phase in ('timing', 'all'):
                timing['rows'].append(tr); run.save(out / 'timing.json', timing)
    run.save(out / 'measurements.json', combined)
    run.save(out / ('complete-' + args.phase + '.json'), {'status': 'complete', 'sources': source_pins,
             'measurements_sha256': run.digest(ws / 'results.json'),
             'artifacts': {name: run.digest(out / name) for name in ('proofs.json', 'timing.json', 'measurements.json', 'quality.json')
                           if (out / name).exists()}})


if __name__ == '__main__': main()
