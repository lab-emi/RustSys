#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Validate selected measured profiles, including incomplete benchmark matrices.

This never upgrades a scout to a repeated benchmark. Reporter memory <128MiB
(<=40 rows). Small models use the existing 4GiB child guards. Full-core model
export is estimated <24GiB; STA <16GiB, guarded at32GiB. All >4GiB jobs share
scale.lock. Every original state/output/event remains in the existing proof.
"""
from pathlib import Path
import argparse
import fcntl

import run
import scaling_verify
import sta_benchmark


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--workspace',type=Path,required=True)
    p.add_argument('--directory',type=Path,required=True)
    p.add_argument('--sta',type=Path,required=True)
    p.add_argument('--design',action='append')
    p.add_argument('--library',action='append')
    p.add_argument('--tool',action='append',choices=('rustsys','yosys'))
    p.add_argument('--phase',choices=('proofs','timing','all'),default='all')
    a=p.parse_args();ws=a.workspace.resolve();out=a.directory.resolve()
    data=run.read_json(ws/'results.json')
    if len(data['rows'])>40:raise ValueError('profile row budget')
    for key in ('input_sha256','binary_sha256'):
        for path,h in data[key].items():
            if run.digest(path)!=h:raise ValueError('changed measured input/tool: '+path)
    out.mkdir(parents=True,exist_ok=False)
    result={'schema':1,'measurement_sha256':run.digest(ws/'results.json'),
        'measurement_repetitions':data['limits']['repetitions'],
        'validator_sha256':run.digest(Path(__file__)),
        'scope':{'initial_states':'arbitrary; checked named state correspondence',
                 'reset_assumptions':'none','input_assumptions':'none'},'rows':[]}
    for row in data['rows']:
        if row['allocated_physical_cores']!=16:continue
        if a.design and row['id'] not in a.design:continue
        if a.library and row['library'] not in a.library:continue
        for tool in a.tool or ('rustsys','yosys'):
            if tool not in row:continue
            measured=row[tool]
            if measured['status']!='ok' or not measured['samples']:
                raise ValueError('incomplete synthesis sample')
            folder=ws/row['id']/row['library']/'16c'/f'{tool}-{len(measured["samples"])}'
            cfg=run.read_json(folder/'config.json');netlist=folder/'netlist.v'
            if run.digest(netlist)!=measured['netlist_sha256']:
                raise ValueError('measured netlist changed')
            if Path(cfg['directory']).resolve()!=folder:
                raise ValueError('sample directory differs from measured configuration')
            dest=out/row['id']/row['library']/tool;dest.mkdir(parents=True)
            entry={'id':row['id'],'library':row['library'],'tool':tool,
                'netlist_sha256':measured['netlist_sha256'],
                'config_sha256':run.digest(folder/'config.json'),
                'area_um2':measured['qor']['area_um2']}
            if a.phase!='timing':
                print('LEC',row['id'],row['library'],tool,flush=True)
                with (run.ROOT/'.work/scale.lock').open('a') as lock:
                    if row['id']=='openc910':fcntl.flock(lock,fcntl.LOCK_EX)
                    proof=scaling_verify.native_case(cfg,dest/'lec',measured['samples'][-1])
                if not scaling_verify.complete(proof) and row['id'] in ('ibex','vexriscv-verilog'):
                    raw=dest/'lec/export/abc-cec.stdout'
                    if raw.exists() and any(s in raw.read_text() for s in ('NOT EQUIVALENT','Verification failed')):
                        raise ValueError('external counterexample requires investigation')
                    proof=scaling_verify.observed_case(cfg,dest/'composed')
                entry['proof']=proof
                entry['proof_complete']=scaling_verify.complete(proof)
                result['rows'].append(entry);run.save(out/'results.json',result)
                if not entry['proof_complete']:raise ValueError('LEC remains unverified: '+str(dest))
                for path,h in proof['source_sha256'].items():
                    if run.digest(path)!=h:raise ValueError('proof input changed')
                if entry['netlist_sha256'] not in proof['source_sha256'].values():
                    raise ValueError('proof omits measured netlist')
            else:result['rows'].append(entry)
            if a.phase!='proofs':
                timing=sta_benchmark.measure(netlist,cfg['library'],cfg['design']['top'],
                    scaling_verify.CLOCK[row['id']],a.sta,dest/'sta',
                    memory_gib=32 if row['id']=='openc910' else 4,seconds=7200,cpu=31)
                entry['timing']=timing
                run.save(out/'results.json',result)
                if timing['status']!='ok':raise ValueError('timing remains unverified: '+str(dest))
                print('OpenSTA',row['id'],row['library'],tool,timing['fmax_mhz'],flush=True)
            run.save(out/'results.json',result)
    if not result['rows']:raise ValueError('no matching 16-core samples')


if __name__=='__main__':main()
