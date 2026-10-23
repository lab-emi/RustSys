#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Balanced, high-resolution, complete-CLI producer comparisons.

Parent <256 MiB: <=1024 samples, <=64 MiB JSON; no design graph. Small
children use 8 GiB, complete OpenC910 uses 32 GiB RAM +1 GiB swap/40 GiB AS.
The existing worker guards wall time, files and workspace size. One job at a
time under scale.lock, fixed physical CPU set. A native monotonic timer wraps
the unchanged _pipeline child, including Python and all four CLI startups,
init/import/mmap/synthesis/full export; excludes resource-guard startup.
No clock/CPU overhead subtraction. Missing metrics never pass.
"""
from pathlib import Path
import argparse
import copy
from datetime import datetime, timezone
import fcntl
import os
import random
import shutil
import subprocess
import sys
import time

import lec
import run
import scaling


def physical_cpus():
    found = {}
    for cpu in sorted(os.sched_getaffinity(0)):
        p = Path(f'/sys/devices/system/cpu/cpu{cpu}/topology')
        key = ((p / 'physical_package_id').read_text().strip(), (p / 'core_id').read_text().strip())
        found.setdefault(key, cpu)
    return list(found.values())


def environment(cpus):
    frequencies = {}
    for cpu in cpus:
        path = Path(f'/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_cur_freq')
        if path.exists(): frequencies[str(cpu)] = int(path.read_text())
    return {'frequencies_khz': frequencies, 'loadavg': Path('/proc/loadavg').read_text().strip(),
            'available_memory_kib': scaling.available_kib()}


def plan(designs, libraries, pairs, warmups, controls, seed):
    if not designs or not libraries or (pairs + warmups + controls) * len(designs) * len(libraries) * 2 > 1024:
        raise ValueError('sample-count budget exceeded')
    rng = random.Random(seed)
    blocks = [('warmup', n) for n in range(warmups)]
    measured = [('comparison', n) for n in range(pairs)] + [('same_binary', n) for n in range(controls)]
    rng.shuffle(measured)
    blocks += measured
    result = []
    for n, (kind, index) in enumerate(blocks):
        rotation = n % len(designs)
        for did in designs[rotation:] + designs[:rotation]:
            for lib in libraries:
                order = ['baseline', 'candidate'] if index % 2 == 0 else ['candidate', 'baseline']
                result.append({'kind': kind, 'round': index, 'id': did, 'library': lib, 'order': order})
    return result


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ('workspace', 'baseline', 'candidate', 'timer'):
        p.add_argument('--' + name, type=Path, required=True)
    p.add_argument('--design', action='append', choices=('ibex', 'picorv32', 'serv', 'vexriscv-verilog', 'openc910'))
    p.add_argument('--library', action='append', choices=('sky130hd_tt', 'gf180mcu7t_tt', 'ez130_8t_tt'))
    p.add_argument('--pairs', type=int, default=20)
    p.add_argument('--warmups', type=int, default=2)
    p.add_argument('--controls', type=int, default=4)
    p.add_argument('--seed', type=int, default=20261023)
    p.add_argument('--timeout', type=int, default=600)
    p.add_argument('--resume', action='store_true')
    a = p.parse_args()
    if not 4 <= a.pairs <= 64 or not 1 <= a.warmups <= 4 or not 2 <= a.controls <= 16:
        p.error('invalid bounded sample counts')
    if not 1 <= a.timeout <= 14400: p.error('invalid wall guard')
    designs = a.design or ['ibex', 'picorv32', 'serv', 'vexriscv-verilog', 'openc910']
    libraries = a.library or ['gf180mcu7t_tt']
    if len(set(designs)) != len(designs) or len(set(libraries)) != len(libraries):
        p.error('duplicate design/library')
    ws = a.workspace.resolve(); timer = a.timer.resolve()
    variants = {'baseline': a.baseline.resolve(), 'candidate': a.candidate.resolve()}
    configs = scaling.configurations(extra_pdks=run.HERE / 'additional-pdks.json')
    cpus = physical_cpus()[:16]
    if len(cpus) != 16: raise ValueError('sixteen physical CPUs required')
    schedule = plan(designs, libraries, a.pairs, a.warmups, a.controls, a.seed)
    sources = {p: run.digest(p) for did in designs for lib in libraries
               for p in [configs[did, lib, 'rustsys']['library'], *configs[did, lib, 'rustsys']['design']['files']]}
    tools = {str(p): run.digest(p) for bins in variants.values() for p in (bins / 'rustsys', bins / 'rusteda')}
    tools[str(timer)] = run.digest(timer)
    expected = {'source_sha256': sources, 'tools_sha256': tools, 'schedule': schedule, 'cpu_affinity': cpus,
                'timeout_s': a.timeout, 'variants': {k: str(v) for k, v in variants.items()}}
    if a.resume:
        data = run.read_json(ws / 'results.json')
        if data['identity'] != expected: raise ValueError('resume identity differs')
    else:
        ws.mkdir(parents=True, exist_ok=False)
        snapshot = ws / 'harness'; snapshot.mkdir()
        for source in run.HERE.iterdir():
            if source.is_file() and source.suffix in ('.py', '.json', '.v'):
                shutil.copy2(source, snapshot / source.name)
        shutil.copy2(run.ROOT / 'crates/verify/rustsys-check/examples/precise_command.rs', snapshot / 'precise_command.rs')
        data = {'schema': 1, 'date': datetime.now(timezone.utc).isoformat(), 'identity': expected,
                'scope': {'physical_cores': 16, 'timer': 'native monotonic duration around unchanged complete pipeline child',
                          'includes': ['pipeline Python startup', 'all four CLI startups', 'init', 'RTL/Liberty import',
                                       'mmap', 'synthesis', 'full Verilog export', 'pipeline child shutdown'],
                          'excludes': ['resource guard startup', 'source preparation', 'LEC', 'STA'],
                          'no_overhead_subtraction': True, 'initial_states': 'arbitrary; unchanged synthesis options',
                          'same_binary_control': 'both labels execute the candidate binary', 'exclusive_host': False,
                          'guard_monitoring': 'campaign scans outside timer; small child has no periodic scan; large child scans only its current case'},
                'harness_sha256': {p.name: run.digest(p) for p in snapshot.iterdir()}, 'blocks': []}
        scaling.atomic_save(ws / 'results.json', data)
    for name, digest in data['harness_sha256'].items():
        if run.digest(ws / 'harness' / name) != digest: raise ValueError('frozen harness changed')
    with (run.ROOT / '.work/scale.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        for number, block in enumerate(schedule):
            if number < len(data['blocks']):
                for e in data['blocks'][number]['samples']:
                    if run.digest(e['netlist']) != e['sample']['netlist_sha256']: raise ValueError('retained netlist changed')
                continue
            result = {**block, 'samples': []}
            for label in block['order']:
                version = 'candidate' if block['kind'] == 'same_binary' else label
                folder = ws / 'cases' / f'case{number:04}-{label}'
                if folder.exists(): folder.rename(folder.with_name(folder.name + '-interrupted'))
                folder.mkdir(parents=True)
                cfg = copy.deepcopy(configs[block['id'], block['library'], 'rustsys'])
                cfg.update(directory=str(folder), threads=16, rustsys=str(variants[version] / 'rustsys'),
                           rusteda=str(variants[version] / 'rusteda'))
                run.save(folder / 'config.json', cfg)
                memory = 32 if block['id'] == 'openc910' else 8
                if scaling.available_kib() < (memory + 2) * 1024**2:
                    raise ValueError('insufficient available host RAM for guarded sample')
                unit = f'rustsys-paired-{os.getpid()}-{number}-{label}.scope'
                # Campaign-wide scans grow with every retained sample. Keep
                # them outside the measured child. Only the large child needs
                # periodic disk protection, limited to its own bounded case.
                lec.tree_budget(ws, max_bytes=256 * 1024**3, max_files=200000)
                before = environment(cpus)
                time.sleep(0.05)
                print('start', number, block['kind'], block['id'], block['library'], block['round'], label, flush=True)
                command = ['systemd-run', '--user', '--scope', '--quiet', '--unit=' + unit,
                           '-p', f'MemoryMax={memory}G', '-p', 'MemorySwapMax=1G', 'taskset', '-c', ','.join(map(str, cpus)),
                           str(timer), str(folder / 'clock.json'), sys.executable, str(ws / 'harness/run.py'),
                           '_pipeline', str(folder / 'config.json')]
                try:
                    sample = run.execute(command, folder, 'synth', seconds=a.timeout, memory_gib=memory + 8,
                                         max_file_bytes=4 * 1024**3, worker_threads=16,
                                         monitor=(lambda: lec.tree_budget(folder, max_bytes=16 * 1024**3, max_files=2048))
                                         if block['id'] == 'openc910' else None)
                finally:
                    subprocess.run(['systemctl', '--user', 'kill', '--signal=KILL', unit], stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL, timeout=10)
                if sample['status'] != 'ok': raise ValueError('sample failed: ' + str(folder))
                lec.tree_budget(ws, max_bytes=256 * 1024**3, max_files=200000)
                clock = run.read_json(folder / 'clock.json')
                if clock.get('exit_code') != 0 or type(clock.get('wall_ns')) is not int or clock['wall_ns'] <= 0:
                    raise ValueError('missing high-resolution completed sample')
                sample['precise_wall_s'] = clock['wall_ns'] / 1e9
                sample['qor'] = run.qos(folder, 'rustsys')
                sample['netlist_sha256'] = run.digest(folder / 'netlist.v')
                report = run.read_json(folder / 'report.json')
                phase = {k: report[k] for k in ('runtime_s', 'phase_s', 'opt_phase_s', 'map_phase_s')}
                entry = {'label': label, 'version': version, 'sample': sample, 'phases': phase,
                         'environment_before': before, 'environment_after': environment(cpus),
                         'config': str(folder / 'config.json'), 'netlist': str(folder / 'netlist.v'),
                         'clock_sha256': run.digest(folder / 'clock.json')}
                run.save(folder / 'sample.json', entry)
                result['samples'].append(entry)
                print('done', number, block['id'], label, f"{sample['precise_wall_s']:.6f}", flush=True)
            data['blocks'].append(result)
            scaling.atomic_save(ws / 'results.json', data)
            if (ws / 'results.json').stat().st_size > 64 * 1024**2: raise ValueError('JSON byte budget')
            for path, digest in tools.items():
                if run.digest(path) != digest: raise ValueError('producer/timer changed')
        for path, digest in sources.items():
            if run.digest(path) != digest: raise ValueError('RTL/PDK changed')
    run.save(ws / 'complete.json', {'status': 'complete', 'results_sha256': run.digest(ws / 'results.json')})


if __name__ == '__main__': main()
