#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Join fixed RustSys controls with the uniform initial-state-preserving reference.

One bounded JSON dataset per input (<128MiB); reporter planning<512MiB RAM.
Hard-linked immutable raw samples avoid duplicating large models. At most20
case directories/160000 files/256GiB output; waits are explicitly bounded.
No timed samples are invented, merged across profiles, or omitted from a case.
"""
from pathlib import Path
import argparse
import copy
import json
import os
import shutil
import time

import lec
import run

IDS={'ibex','picorv32','serv','vexriscv-verilog','openc910'}
LIBS={'sky130hd_tt','gf180mcu7t_tt'}


def loaded(path):
    try:return run.read_json(path/'results.json') if (path/'results.json').exists() else {'rows':[]}
    except json.JSONDecodeError:return {'rows':[]}


def ready(data,reference):
    rows={(r['id'],r['library'],r['allocated_physical_cores']):r for r in data['rows']}
    for did in IDS:
        for lib in LIBS:
            for n in ((16,) if reference else (1,16)):
                r=rows.get((did,lib,n),{}).get('yosys' if reference else 'rustsys',{})
                if r.get('status')!='ok' or len(r.get('samples',[]))!=3:return False
    return True


def main():
    p=argparse.ArgumentParser(description=__doc__)
    for name in ('rustsys','reference','output'):p.add_argument('--'+name,type=Path,required=True)
    p.add_argument('--wait-seconds',type=int,default=86400)
    a=p.parse_args()
    if not 1<=a.wait_seconds<=86400:p.error('invalid wait budget')
    out=a.output.resolve();out.mkdir(parents=True,exist_ok=False)
    run.save(out/'results.json',{'schema':1,'status':'waiting_for_complete_measurements','rows':[]})
    end=time.monotonic()+a.wait_seconds
    print('Waiting for complete uniform-profile datasets',flush=True)
    while time.monotonic()<end:
        rust,reference=loaded(a.rustsys),loaded(a.reference)
        if ready(rust,False) and ready(reference,True):break
        time.sleep(5)
    else:raise ValueError('uniform-profile measurement matrix incomplete at the wait budget')
    if rust['input_sha256']!=reference['input_sha256']:
        raise ValueError('reference RTL or libraries differ')
    for source in (rust,reference):
        if source['limits']['repetitions']!=3 or source['limits']['warmups']!=1:
            raise ValueError('join requires one warmup and three measured runs')
    for executable in reference.get('reference_executables',[]):
        if run.digest(executable['path'])!=executable['sha256']:
            raise ValueError('opaque reference executable changed')
    by={(r['id'],r['library']):r for r in reference['rows'] if r['allocated_physical_cores']==16}
    data=copy.deepcopy(rust);data['rows']=[]
    # An unused RustSys executable in an earlier reference campaign is not a
    # Yosys dependency. Pin only executables used by each selected pipeline;
    # retain both complete source manifests for audit, without inventing reruns.
    data['binary_sha256']={}
    data['reference_executables']=copy.deepcopy(reference.get('reference_executables',{}))
    data['source_campaigns']={str(x.resolve()):run.digest(x/'results.json') for x in (a.rustsys,a.reference)}
    data['scope'].update(yosys_flow='same initial-state-preserving structural-memory/default-ABC library flow for every design',
                         yosys_openc910_profile='same default ABC library mapping as smaller designs; no cut-count/area-round reduction',
                         other_yosys_profiles='same initial-state-preserving reference; no reset or initialization choices',
                         selected_matrix='RustSys1/16physicalcores;Yosys16physicalcores;onewarmup+three measuredruns',
                         legacy_data='earlier standard-flow and bounded-map reference samples remain in their original datasets; not mixed into these medians')
    data['reference_campaign_limits']=reference['limits']
    if 'yosys_timing' in reference['limits']:
        data['limits']['yosys_timing']=copy.deepcopy(reference['limits']['yosys_timing'])
        data['scope']['yosys_flow']='initial-state-preserving default ABC mapping with public timing constraints and buffer/upsize/dnsize'
    for original in rust['rows']:
        row={k:copy.deepcopy(v) for k,v in original.items() if k not in ('rustsys','yosys')}
        row['rustsys']=copy.deepcopy(original['rustsys'])
        if row['allocated_physical_cores']==16:row['yosys']=copy.deepcopy(by[row['id'],row['library']]['yosys'])
        for tool in ('rustsys','yosys'):
            if tool not in row:continue
            source=a.reference if tool=='yosys' else a.rustsys
            for rep in (0,1,2,3):
                relative=Path(row['id'])/row['library']/f"{row['allocated_physical_cores']}c"/f'{tool}-{rep}'
                origin=source/relative;target=out/relative
                sample=run.read_json(origin/'sample.json')
                cfg=run.read_json(origin/'config.json')
                metadata=reference if tool=='yosys' else rust
                if cfg['tool']!=tool:raise ValueError('sample tool identity differs')
                for name in (('yosys',) if tool=='yosys' else ('rustsys','rusteda')):
                    executable=str(Path(cfg[name]).resolve());digest=run.digest(executable)
                    if metadata['binary_sha256'].get(executable)!=digest:
                        raise ValueError('selected producer binary changed: '+executable)
                    data['binary_sha256'][executable]=digest
                if sample['status']!='ok' or run.digest(origin/'netlist.v')!=sample['netlist_sha256']:raise ValueError('raw repeated sample changed')
                if rep and sample['netlist_sha256']!=row[tool]['netlist_sha256']:raise ValueError('repeated measured netlist differs')
                target.parent.mkdir(parents=True,exist_ok=True)
                shutil.copytree(origin,target,copy_function=os.link)
                lec.tree_budget(out,max_bytes=256*1024**3,max_files=160000)
        data['rows'].append(row)
    run.save(out/'ready.json',data)
    (out/'ready.json').replace(out/'results.json')
    run.save(out/'complete.json',{'status':'complete','selected_rows':20,'source_campaigns':data['source_campaigns']})
    print('Uniform-profile matrix ready:',out,flush=True)


if __name__=='__main__':main()
