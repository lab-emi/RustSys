#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Join actually measured RustSys/Yosys 1/16-core samples with immutable provenance.

At most8 input manifests/40 tool allocations/160 samples; <=128MiB manifest
bytes and256GiB/200K retained files. Hardlinks share immutable measured payloads.
No measurements are inferred from an executable being mostly single-threaded.
"""
from pathlib import Path
import argparse
import copy
import os
import shutil

import lec
import run
from scaling_join import IDS, LIBS


def combine(rustsys, references, output):
    if not 1 <= len(references) <= 8:
        raise ValueError('expected one to eight reference campaigns')
    sources = [rustsys, *references]
    if sum((p/'results.json').stat().st_size for p in sources) > 128 * 1024**2:
        raise ValueError('manifest memory budget exceeded')
    manifests = {p: run.read_json(p/'results.json') for p in sources}
    selected = {}
    for source, data in manifests.items():
        tool = 'rustsys' if source == rustsys else 'yosys'
        if data['limits']['warmups'] != 1 or data['limits']['repetitions'] != 3:
            raise ValueError('every selected campaign requires one warmup and three measured samples')
        for row in data['rows']:
            key = row['id'], row['library'], row['allocated_physical_cores'], tool
            if tool not in row or key[2] not in (1, 16):
                continue
            if key in selected:
                raise ValueError('ambiguous duplicated measurement allocation: '+str(key))
            result = row[tool]
            if result['status'] != 'ok' or len(result['samples']) != 3:
                raise ValueError('incomplete measured allocation: '+str(key))
            selected[key] = source, row
    expected = {(d, l, n, t) for d in IDS for l in LIBS for n in (1,16) for t in ('rustsys','yosys')}
    if set(selected) != expected:
        raise ValueError('unified matrix is incomplete: '+str(sorted(expected-set(selected))))
    base = manifests[rustsys]
    data = copy.deepcopy(base)
    data['rows'] = []
    data['binary_sha256'] = {}
    data['source_campaigns'] = {str(p.resolve()): run.digest(p/'results.json') for p in sources}
    data['scope']['selected_matrix'] = 'RustSys and Yosys each at1and16physicalcores; one warmup and three measured runs per allocation'
    data['scope']['required_reference_cores'] = [1,16]
    data['scope'].update(yosys_flow='arbitrary-initial-state-full-map-v3 for every design and CPU allocation',
        yosys_openc910_profile='same initial-state-preserving structural memory lowering and default ABC mapping as smaller designs',
        other_yosys_profiles='same arbitrary-state v3 flow; no reset/input assumptions or timing constraints')
    data['reference_executables'] = []
    data['reference_campaign_limits'] = {str(p.resolve()): manifests[p]['limits'] for p in references}
    for source in references:
        for exe in manifests[source].get('reference_executables',[]):
            if run.digest(exe['path']) != exe['sha256']:
                raise ValueError('opaque reference executable changed')
            if exe not in data['reference_executables']:
                data['reference_executables'].append(copy.deepcopy(exe))
    output.mkdir(parents=True, exist_ok=False)
    for did, library, cores in sorted({key[:3] for key in expected}):
        original = selected[did, library, cores, 'rustsys'][1]
        row = {k: copy.deepcopy(v) for k,v in original.items() if k not in ('rustsys','yosys')}
        for tool in ('rustsys','yosys'):
            source, origin_row = selected[did, library, cores, tool]
            meta = manifests[source]
            row[tool] = copy.deepcopy(origin_row[tool])
            for rep in range(4):
                relative = Path(did)/library/f'{cores}c'/f'{tool}-{rep}'
                folder = source/relative
                cfg = run.read_json(folder/'config.json')
                sample = run.read_json(folder/'sample.json')
                if cfg['tool'] != tool or cfg['threads'] != cores:
                    raise ValueError('tool/CPU allocation differs from the claimed measurement')
                for name in (('rustsys','rusteda') if tool == 'rustsys' else ('yosys',)):
                    exe = str(Path(cfg[name]).resolve())
                    digest = run.digest(exe)
                    if meta['binary_sha256'].get(exe) != digest:
                        raise ValueError('selected producer binary changed')
                    data['binary_sha256'][exe] = digest
                for path in [cfg['library'], *cfg['design']['files']]:
                    digest = run.digest(path)
                    if meta['input_sha256'].get(path) != digest or base['input_sha256'].get(path) != digest:
                        raise ValueError('selected source/library differs')
                if (sample['status'] != 'ok' or run.digest(folder/'netlist.v') != sample['netlist_sha256']
                        or sample['netlist_sha256'] != row[tool]['netlist_sha256']):
                    raise ValueError('incomplete/nondeterministic sample')
                if tool == 'yosys' and (cfg.get('yosys_flow') != 'arbitrary-initial-state-full-map-v3'
                        or cfg.get('yosys_bounded_map') is not False or cfg.get('yosys_abc_fast') is not False
                        or cfg.get('yosys_delay_ff_optimization') is not True or cfg.get('yosys_timing') is not None):
                    raise ValueError('reference mapping profile differs')
                target = output/relative
                target.parent.mkdir(parents=True,exist_ok=True)
                shutil.copytree(folder,target,copy_function=os.link)
                lec.tree_budget(output,max_bytes=256*1024**3,max_files=200000)
        data['rows'].append(row)
    by = {(r['id'],r['library'],r['allocated_physical_cores']): r for r in data['rows']}
    for did in IDS:
        for library in LIBS:
            for tool in ('rustsys','yosys'):
                one, sixteen = (by[did,library,n][tool] for n in (1,16))
                if one['netlist_sha256'] != sixteen['netlist_sha256'] or one['qor'] != sixteen['qor']:
                    raise ValueError('1/16 outputs require separate proof/timing evidence: '+str((did,library,tool)))
    run.save(output/'results.json',data)
    run.save(output/'complete.json',{'status':'complete','tool_allocations':40,'source_campaigns':data['source_campaigns']})
    print('Complete measured RustSys/Yosys1/16matrix:',output,flush=True)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--rustsys',type=Path,required=True)
    p.add_argument('--reference',type=Path,action='append',required=True)
    p.add_argument('--output',type=Path,required=True)
    a = p.parse_args()
    combine(a.rustsys.resolve(),[x.resolve() for x in a.reference],a.output.resolve())
