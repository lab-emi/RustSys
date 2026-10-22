#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Bounded exact AIGER output projections, independently checked by ABC CEC.

Planning: source AND/output arrays ~281 MiB, <=1M local DAG entries *160 B,
bounded buffers and metadata <1 GiB. Slice production and each ABC child have
4 GiB physical projection guards. Up to four ABC workers use 1 GiB each
(planning <1.5 GiB total solver/parent RSS); the work queue is capped at 2W.
The raw independent RTL/state theorem
and its immutable source hashes are retained; no reset/input choices are added.
"""
from pathlib import Path
from array import array
import argparse
import json
import os
import hashlib
import subprocess
import sys
import time
from concurrent.futures import ProcessPoolExecutor, wait, FIRST_COMPLETED
import multiprocessing

import run
import transition
import large_lec

METHOD='abc-independent-rtl-event-sliced-v1'


def bounded(argv,directory,name,seconds,cpu,memory_gib=4):
    unit='rustsys-slice-'+hashlib.sha256((str(directory)+name).encode()).hexdigest()[:16]+'.scope'
    try:
        return run.execute(['systemd-run','--user','--scope','--quiet','--unit='+unit,
            '-p',f'MemoryMax={memory_gib}G','-p','MemorySwapMax=512M','taskset','-c',str(cpu),*map(str,argv)],
            directory,name,seconds=seconds,memory_gib=8)
    finally:
        subprocess.run(['systemctl','--user','kill','--signal=KILL',unit],
            stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)


def check_slice(job):
    folder,destination,row,original_inputs,abc,seconds,cpu,memory_gib=job
    path=folder/f"slice{row['id']:05}";n=row['cec_outputs']
    if (path/'output_ids.u32le').stat().st_size>4*16384 or (path/'input_ids.u32le').stat().st_size>4*2_000_000:
        raise ValueError('slice interface allocation budget')
    ids=array('I');ids.frombytes((path/'output_ids.u32le').read_bytes())
    inputs=array('I');inputs.frombytes((path/'input_ids.u32le').read_bytes())
    if sys.byteorder!='little':ids.byteswap();inputs.byteswap()
    if (len(ids)!=n or list(ids)!=sorted(set(ids)) or not all(row['start']<=x<row['end'] for x in ids)
            or len(inputs)!=row['inputs'] or len(set(inputs))!=len(inputs)
            or not all(1<=x<=original_inputs for x in inputs)):
        raise ValueError('invalid slice projection interface')
    for name in ('gold.aig','gate.aig'):
        with (path/name).open('rb') as f:h=f.readline(256).decode().split()
        if len(h)!=6 or h[0]!='aig':raise ValueError('invalid slice header')
        m,i,l,o,a=map(int,h[1:])
        if l or i!=row['inputs'] or o!=n or a!=row['ands'] or m!=i+a:raise ValueError('slice header differs from coverage')
    before={name:run.digest(path/name) for name in ('gold.aig','gate.aig','input_ids.u32le','output_ids.u32le')}
    checked=destination/f"check{row['id']:05}";checked.mkdir()
    command=transition.cec_command(transition.Limits(),seconds,path/'gold.aig',path/'gate.aig')
    r=bounded([abc,'-c',command],checked,'abc',seconds+5,cpu,memory_gib)
    yes=r['status']=='ok' and transition.cec_equivalent((checked/'abc.stdout').read_text())
    if before!={name:run.digest(path/name) for name in before}:raise ValueError('slice changed during external proof')
    item={'id':row['id'],'start':row['start'],'end':row['end'],'outputs':n,'proven':yes,'measurement':r,'sha256':before}
    run.save(checked/'certificate.json',item);return item


def compare(source,destination,slicer,abc,cpu=8,seconds=300,prepared=None,jobs=1):
    if prepared is not None:
        raise ValueError('fresh projections from the checked source AIGERs are required; cached projection reuse is disabled')
    if not 1<=jobs<=4:raise ValueError('CEC worker budget must be between one and four')
    cores=list(range(cpu,cpu+jobs))
    if not set(cores)<=os.sched_getaffinity(0):raise ValueError('CEC CPU budget is outside host affinity')
    destination.mkdir(parents=True,exist_ok=False);steps=[];start=time.monotonic()
    files=[source/'gold.aig',source/'gate.aig']
    hashes={str(p):run.digest(p) for p in files}
    tool_hashes={str(p):run.digest(p) for p in (slicer,abc)}
    harness_hashes={name:run.digest(run.HERE/name) for name in ('slice_lec.py','run.py','transition.py')}
    folder=prepared or destination/'slices'
    if prepared is None:
        r=bounded([slicer,*files,folder],destination,'project',1800,cpu)
        steps.append(r)
        if r['status']!='ok':return {'equivalent':False,'diagnostic':'bounded slice projection failed','steps':steps}
    if (folder/'manifest.json').stat().st_size>32*1024**2:raise ValueError('projection manifest byte budget')
    plan=run.read_json(folder/'manifest.json')
    if plan['status']!='prepared':return {'equivalent':False,'diagnostic':plan.get('diagnostic','projection incomplete'),'steps':steps}
    if len(plan['slices'])>65536:raise ValueError('projection batch-count budget')
    cursor=0;direct=0;structural=0;projected=0;failed=[];certificates=[];pending_jobs=[]
    for index,row in enumerate(plan['slices']):
        if row['id']!=index:raise ValueError('projection batch ids are not unique and ordered')
        if row['start']!=cursor or not row['end']>cursor:raise ValueError('overlap or gap in output coverage')
        cursor=row['end'];n=row['cec_outputs'];direct+=row['direct'];structural+=row['structural'];projected+=n
        if row['end']-row['start']!=n+row['direct']+row['structural']:raise ValueError('incomplete slice accounting')
        if n:
            # Each solver handles <=1M projected DAG nodes and 2M conflicts.
            # Four workers plan <1.5 GiB combined RSS; each parallel child has
            # a 1 GiB physical guard. Projection runs separately at 4 GiB.
            pending_jobs.append((folder,destination,row,plan['original_inputs'],abc,seconds,
                                 ','.join(map(str,cores)) if jobs>1 else cpu,1 if jobs>1 else 4))
    def record(item):
        certificates.append(item)
        if not item['proven']:failed.append(item['id'])
        if len(certificates)%64==0:print('ABC batches',len(certificates),'/',len(pending_jobs),'failed',len(failed),flush=True)
    if jobs==1:
        for job in pending_jobs:record(check_slice(job))
    else:
        # Spawn avoids fork/preexec hazards in a threaded parent. Bound the
        # outstanding work queue to twice the explicit worker budget.
        with ProcessPoolExecutor(max_workers=jobs,mp_context=multiprocessing.get_context('spawn')) as pool:
            iterator=iter(pending_jobs);active=set()
            while True:
                while len(active)<2*jobs:
                    try:job=next(iterator)
                    except StopIteration:break
                    active.add(pool.submit(check_slice,job))
                if not active:break
                done,active=wait(active,return_when=FIRST_COMPLETED)
                for future in done:record(future.result())
    certificates.sort(key=lambda item:item['id']);failed.sort()
    if len(certificates)!=len(pending_jobs):raise ValueError('CEC batch coverage is incomplete')
    if cursor!=plan['original_outputs'] or cursor!=direct+structural+projected:raise ValueError('original outputs are not completely covered')
    unchanged=hashes=={str(p):run.digest(p) for p in files} and tool_hashes=={str(p):run.digest(p) for p in (slicer,abc)}
    unchanged=unchanged and harness_hashes=={name:run.digest(run.HERE/name) for name in harness_hashes}
    if not unchanged:raise ValueError('source AIGER or executable changed')
    result={'equivalent':not failed,'original_outputs':cursor,'direct_identities':direct,'structural_identities':structural,
        'external_outputs':projected,'failed_slices':failed,'slices':certificates,'plan':plan,
        'source_sha256':hashes,'tool_sha256':tool_hashes,'source_artifacts_unchanged':unchanged,
        'harness_sha256':harness_hashes,'fresh_source_projection':True,
        'coverage_checked':True,'interface_checked':True,'projection':'exact original cone; only unused inputs removed and variables consistently renumbered',
        'wall_s':time.monotonic()-start,'steps':steps,'workers':jobs,
        'limits':{'solver_memory_gib':1 if jobs>1 else 4,'solver_swap_mib':512,'cpu_affinity':cores,'max_pending_jobs':2*jobs}}
    run.save(destination/'comparison.json',result);return result


def complete(proof):
    c=proof.get('certificate',{});base=c.get('source_export',{});m=base.get('manifest',{});comparison=c.get('comparison',{})
    k=m.get('obligation_kinds',{});n=m.get('gate_state_elements',0)
    return (proof.get('method')==METHOD and proof.get('status')=='ok' and proof.get('verdict')=='proven'
        and comparison.get('equivalent') is True and comparison.get('coverage_checked') is True
        and comparison.get('interface_checked') is True and comparison.get('source_artifacts_unchanged') is True
        and not comparison.get('failed_slices') and comparison.get('original_outputs')==sum(k.values())
        and comparison.get('external_outputs',0)>0 and all(x.get('proven') is True for x in comparison.get('slices',[]))
        and base.get('artifact_hashes_checked') is True and base.get('state_relation_coverage_checked') is True
        and base.get('gate_state_inventory_checked',base.get('producer_state_count_checked')) is True
        and m.get('all_gate_initial_states_covered') is True
        and m.get('all_rtl_inputs_and_states_quantified') is True and m.get('state_pairs')==n and n>0
        and m.get('reset_assumptions')==[] and m.get('initial_state_choices')==[] and m.get('case_assumptions')==[]
        and k.get('clock',0)+k.get('enable',0)==n and k.get('async-clear')==n and k.get('async-preset')==n
        and k.get('data',0)+k.get('next-state',0)==n and k.get('output',0)>0
        and proof.get('scope',{}).get('reset_assumptions')=='none')


def main(args):
    original=run.read_json(args.original/'combined-results.json');args.workspace.mkdir(exist_ok=False)
    for row in original['rows']:
        for tool in ('rustsys','yosys'):
            prior=row[tool]['equivalence'];source=args.original/row['library']/tool
            if not (source/'obligations/manifest.json').exists():continue
            export=prior.get('certificate',{})
            if not export.get('artifact_hashes_checked') or not export.get('state_relation_coverage_checked'):raise ValueError('source export preconditions not verified')
            manifest=export['manifest'];relation=large_lec.verify_relation(source/'obligations',manifest)
            if any(export[k]!=v for k,v in relation.items()):raise ValueError('original state relation changed')
            for name in ('gold','gate'):
                if run.digest(source/'obligations'/(name+'.aig'))!=export[name+'_sha256']:raise ValueError('original obligation file changed')
            folder=args.workspace/row['library']/tool;prepared=args.prepared if row['library']=='sky130hd_tt' else None
            compared=compare(source/'obligations',folder,args.slicer,args.abc,args.cpu,args.seconds,prepared,jobs=args.jobs)
            result={'method':METHOD,'status':'ok' if compared['equivalent'] else 'unverified','verdict':'proven' if compared['equivalent'] else 'unverified',
                'partitions':len(compared.get('slices',[])),'scope':prior['scope'],'source_sha256':prior['source_sha256'],'tools':prior['tools'],
                'certificate':{'source_export':export,'comparison':compared}}
            if result['verdict']=='proven' and not complete(result):raise ValueError('incomplete sliced certificate')
            run.save(folder/'result.json',result);row[tool]['equivalence']=result
            run.save(args.workspace/'combined-results.json',original);print(row['library'],tool,result['verdict'],flush=True)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--original',type=Path,required=True);p.add_argument('--workspace',type=Path,required=True)
    p.add_argument('--slicer',type=Path,default=run.ROOT/'target/release/examples/aig_slices');p.add_argument('--abc',type=Path,required=True)
    p.add_argument('--prepared',type=Path);p.add_argument('--cpu',type=int,default=8);p.add_argument('--seconds',type=int,default=300);p.add_argument('--jobs',type=int,choices=range(1,5),default=1)
    a=p.parse_args()
    for key in ('original','workspace','slicer','abc','prepared'):
        if getattr(a,key)is not None:setattr(a,key,getattr(a,key).resolve())
    available=next(int(line.split()[1]) for line in Path('/proc/meminfo').read_text().splitlines() if line.startswith('MemAvailable:'))
    if available<6*1024**2:p.error('insufficient available RAM for 4 GiB proof guards')
    main(a)
