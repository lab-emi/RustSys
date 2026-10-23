#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Attach checked LEC provenance and independent OpenSTA to scaling measurements.

Reporter<256MiB:20rows and bounded JSON. Changed small netlists use a4GiB-RAM
(+1GiB swap)/12GiB-AS external gate bridge, one solver; expected<2GiB RSS.
Every gate/state/output obligation remains. Full-core STA is serialized by
scale.lock with its own32GiB guard and16GiB upfront working estimate.
No benchmark timer includes validation. Missing/partial results never pass.
"""
from pathlib import Path
import argparse
import copy
import fcntl
import json
import os
import shutil
import subprocess
import sys
import time
from argparse import Namespace

import run
import report
import slice_lec
import sta_benchmark
import transition
import large_lec
import observed_proof
import evidence_reuse
import archive_json

IDS={'ibex','picorv32','serv','vexriscv-verilog','openc910'}
LIBS={'sky130hd_tt','gf180mcu7t_tt'}
CLOCK={'ibex':'clk_i','picorv32':'clk','serv':'clk','vexriscv-verilog':'clk','openc910':'forever_cpuclk'}
METHOD='measured-netlist-transitive-lec-v1'


def complete(proof):
    if proof.get('method')==observed_proof.METHOD:return observed_proof.complete(proof)
    if proof.get('method')!=METHOD:
        return report.complete_external_proof(proof) or slice_lec.complete(proof) or large_lec.complete(proof)
    c=proof.get('certificate',{});old=c.get('original',{});bridge=c.get('bridge',{})
    hashes=set(bridge.get('source_sha256',{}).values())
    return (proof.get('status')=='ok' and proof.get('verdict')=='proven'
        and report.complete_external_proof(old) and report.complete_external_proof(bridge)
        and c.get('old_netlist_sha256') in hashes and c.get('new_netlist_sha256') in hashes
        and c.get('library_sha256') in hashes and c.get('source_and_library_binding_checked') is True
        and proof.get('scope',{}).get('reset_assumptions')=='none')


def ready(data):
    rows={(r['id'],r['library'],r['allocated_physical_cores']):r for r in data['rows']}
    reference_cores=data.get('scope',{}).get('required_reference_cores',[16])
    return all((did,lib,n) in rows and rows[did,lib,n].get('rustsys',{}).get('status')=='ok'
        and len(rows[did,lib,n]['rustsys'].get('samples',[]))==3
        and (n not in reference_cores or (rows[did,lib,n].get('yosys',{}).get('status')=='ok'
             and len(rows[did,lib,n]['yosys'].get('samples',[]))==3))
        for did in IDS for lib in LIBS for n in (1,16))


def wait_ready(workspace,seconds):
    end=time.monotonic()+seconds
    print('Waiting for complete3-run measurements before validation',flush=True)
    while time.monotonic()<end:
        try:data=run.read_json(workspace/'results.json')
        except json.JSONDecodeError:
            time.sleep(1)
            continue
        if ready(data):return data
        time.sleep(5)
    raise ValueError('complete measurement matrix was not available within the wait budget')


def bridge_worker(cfg_path,directory):
    cfg=run.read_json(Path(cfg_path));root=run.ROOT
    suite=root/'.work/lec-followup/oss-cad-20261014/oss-cad-suite/bin'
    result=transition.prove(cfg,Path(directory),str(suite/'yosys'),str(suite/'eqy'),str(suite/'yosys-abc'),seconds=3600,memory_gib=12)
    result['limits'].update(physical_memory_gib=4,address_space_gib=12,swap_gib=1)
    run.save(Path(directory)/'result.json',result)
    return 0 if report.complete_external_proof(result) else 2


def native_case(cfg,dest,sample,event_checker=None):
    """All original event obligations, with an exact-output projection fallback.

    A counterexample is never retried as if it were only a resource limit.
    Projection changes neither logic nor assumptions and covers every output.
    """
    dest=Path(dest);big=cfg['design']['id']=='openc910'
    args=Namespace(checker=Path(event_checker) if event_checker else run.ROOT/'target/release/examples/check_config',
        rusteda=run.ROOT/'target/release/rusteda',
        abc=run.ROOT/'.work/lec-followup/oss-cad-20261014/oss-cad-suite/bin/yosys-abc',
        cpu=30,model_seconds=7200 if big else 1800,seconds=7200 if big else 600,
        memory_gib=24 if big else 4,swap_gib=1,export_only=big,
        reimport_netlists=True,require_full_core=big)
    if big:
        available=next(int(x.split()[1]) for x in Path('/proc/meminfo').read_text().splitlines() if x.startswith('MemAvailable:'))
        if available<(args.memory_gib+2)*1024**2:
            raise ValueError('insufficient available host RAM for full-core model guard')
    export=dest/'export'
    proof=large_lec.prove(cfg,export,args,sample)
    if large_lec.complete(proof):
        run.save(dest/'result.json',proof)
        return proof
    raw=export/'abc-cec.stdout'
    if raw.exists() and ('NOT EQUIVALENT' in raw.read_text() or 'Verification failed' in raw.read_text()):
        run.save(dest/'result.json',proof)
        return proof
    c=proof.get('certificate',{})
    if not c.get('artifact_hashes_checked') or not c.get('state_relation_coverage_checked'):
        run.save(dest/'result.json',proof)
        return proof
    compared=slice_lec.compare(export/'obligations',dest/'projected',
        run.ROOT/'target/release/examples/aig_slices',args.abc,
        cpu=16 if big else 30,seconds=900,jobs=4 if big else 1,solver=cfg.get('cec_solver','default'))
    yes=compared.get('equivalent') is True
    result={'method':slice_lec.METHOD,'status':'ok' if yes else 'unverified',
        'verdict':'proven' if yes else 'unverified','partitions':len(compared.get('slices',[])),
        'scope':proof['scope'],'source_sha256':proof['source_sha256'],'tools':proof['tools'],
        'certificate':{'source_export':c,'comparison':compared}}
    if yes and not slice_lec.complete(result):raise ValueError('incomplete projected event certificate')
    run.save(dest/'result.json',result)
    return result


def observed_case(cfg,dest,observation_checker=None,arisca=None,event_checker=None):
    """Run the complete observed arithmetic proof in an aggregate bounded worker."""
    dest=Path(dest)
    if (dest/'result.json').exists():return observed_proof.validate(run.read_json(dest/'result.json'))
    frozen=run.ROOT/'.work/performance-20261020/binaries/check_probe'
    observation_checker=observation_checker or (frozen if frozen.exists() else run.ROOT/'target/release/examples/check_probe')
    if arisca is None:
        manifest=run.ROOT/'.work/lec-followup/arithmetic-tools-final.json'
        arisca=run.read_json(manifest).get('arisca',{}).get('path') if manifest.exists() else shutil.which('arisca')
    if not arisca or not Path(arisca).is_file():raise ValueError('Arisca missing; provide --arisca for complete arithmetic LEC')
    if not Path(observation_checker).is_file():raise ValueError('observation checker missing; build check_probe or provide --observation-checker')
    available=next(int(x.split()[1]) for x in Path('/proc/meminfo').read_text().splitlines() if x.startswith('MemAvailable:'))
    if available<6*1024**2:raise ValueError('insufficient available RAM for observed proof guard')
    dest.parent.mkdir(parents=True,exist_ok=True)
    path=dest.parent/'observed-config.json';run.save(path,cfg)
    unit=f'rustsys-observed-validation-{os.getpid()}.scope'
    command=['systemd-run','--user','--scope','--quiet','--unit='+unit,'-p','MemoryMax=4G','-p','MemorySwapMax=1G',
        'taskset','-c','22',sys.executable,str(run.HERE/'observed_proof.py'),'--config',str(path),'--directory',str(dest),
        '--checker',str(Path(observation_checker).resolve()),'--arisca',str(Path(arisca).resolve())]
    if event_checker:command+=['--event-checker',str(Path(event_checker).resolve())]
    try:value=run.execute(command,dest.parent,'observed-worker',seconds=21630,memory_gib=12,max_file_bytes=128*1024**2)
    finally:subprocess.run(['systemctl','--user','kill','--signal=KILL',unit],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    if value['status']!='ok':raise ValueError('observed arithmetic proof incomplete: '+str(dest))
    return observed_proof.validate(run.read_json(dest/'result.json'))


def verify_proofs(workspace,directory,data,observation_checker=None,arisca=None,reuse=()):
    reusable=evidence_reuse.load(reuse,'proof')
    baseline={}
    for name in ('2026-10-20-lec-complete','2026-10-20-openc910-verified'):
        r=archive_json.read(run.HERE/'results'/name/'results.json')
        for row in r['rows']:baseline[row['design'],row['library']]=(row,r)
    old_root=run.ROOT/'.work/priority-benchmark/current-source-final'
    rows={(r['id'],r['library'],r['allocated_physical_cores']):r for r in data['rows']}
    result={'schema':1,'scope':{'reset_assumptions':'none','initial_states':'arbitrary implementation state; checked state correspondence',
        'unknowns':'native X/Z-data/undriven-zero RTL event model, external ABC CEC; all designs use the same semantic contract'},'rows':[]}
    for row in data['rows']:
        if row['allocated_physical_cores']!=16:continue
        did,lib=row['id'],row['library'];prior,prior_dataset=baseline[row['design'],lib]
        entry={'id':did,'design':row['design'],'library':lib}
        for tool in ('rustsys','yosys'):
            folder=workspace/did/lib/'16c'/f'{tool}-3';cfg=run.read_json(folder/'config.json')
            netlist=folder/'netlist.v';digest=run.digest(netlist)
            if any(s['netlist_sha256']!=digest for s in row[tool]['samples']):raise ValueError('measured repeated netlist bytes differ')
            if any(run.digest(folder.parent/f'{tool}-{rep}'/'netlist.v')!=digest for rep in (1,2,3)):raise ValueError('a measured repeated netlist file changed')
            if run.digest(cfg['library'])!=prior['library_sha256']:raise ValueError('PDK differs from certified baseline')
            thread_control=tool in rows[did,lib,1]
            if thread_control:
                one=rows[did,lib,1][tool]
                if one['netlist_sha256']!=digest or one['qor']!=row[tool]['qor']:
                    raise ValueError('thread counts changed the netlist or area')
                for rep in (1,2,3):
                    path=workspace/did/lib/'1c'/f'{tool}-{rep}'/'netlist.v'
                    if run.digest(path)!=digest:raise ValueError('one-core measured netlist changed')
            reused=evidence_reuse.proof(reusable,(did,lib,tool),cfg,netlist,complete)
            if reused is not None:
                reused['reuse_binding'].update(all_three_repetitions_checked=True,thread_control_identical=thread_control)
                entry[tool]=reused
                print('Reused exact-byte LEC:',did,lib,tool,flush=True)
                continue
            original=prior[tool]['equivalence']
            if not complete(original):raise ValueError('original certificate is not complete')
            old_sha=prior[tool]['samples'][-1]['netlist_sha256']
            # Bind every current RTL source and library to the original certificate.
            original_inputs={p.replace('$RUSTSYS_ROOT',str(run.ROOT)):h for p,h in original.get('source_sha256',{}).items()}
            if not original_inputs:
                # Published EQY receipts bind the netlist at publication; their
                # checked RTL hashes live in the same published dataset's inputs.
                if original.get('measured_netlist_sha256')!=old_sha or not prior_dataset.get('proof_publication'):
                    raise ValueError('original publication lacks a checked source binding')
                desc=next(x for x in prior_dataset['inputs'] if x['id']==did)
                original_inputs={str(Path(f).resolve()):desc['sha256'][Path(f).name] for f in cfg['design']['files']}
                original_inputs[str(Path(cfg['library']).resolve())]=prior['library_sha256']
            for source in [cfg['library'],*cfg['design']['files']]:
                if original_inputs.get(str(Path(source).resolve()))!=run.digest(source):raise ValueError('original proof does not bind the measured RTL/PDK')
            if did!='openc910' or digest!=old_sha:
                # Fresh proof of the tool's documented two-valued RTL model;
                # export is incomplete until external ABC checks every event.
                dest=directory/'native'/did/lib/tool
                prototype=run.ROOT/'.work/performance-20261020/proofs'/('ibex-sky-native-probe' if did=='ibex' else 'vex-sky-native-probe')
                if (tool=='rustsys' and not dest.exists() and lib=='sky130hd_tt' and did in ('ibex','vexriscv-verilog')
                        and (prototype/'result.json').exists()):
                    trial=run.read_json(prototype/'result.json')
                    if large_lec.complete(trial) and trial['source_sha256'].get(str(netlist))==digest:
                        shutil.copytree(prototype,dest)
                if tool=='yosys' and did in ('ibex','vexriscv-verilog'):
                    # A real counterexample must be investigated; budget-only
                    # failures can be replaced by a complete stronger method.
                    if any('Networks are NOT EQUIVALENT.' in p.read_text() for p in
                            (dest/'abc-cec.stdout',dest/'export/abc-cec.stdout') if p.exists()):
                        raise ValueError('native counterexample requires investigation: '+str(dest))
                    proof=observed_case(cfg,directory/'composed'/did/lib/tool,observation_checker,arisca)
                elif (dest/'result.json').exists():
                    proof=run.read_json(dest/'result.json')
                else:
                    print('Proving fresh RTL event model:',did,lib,tool,flush=True)
                    if did=='openc910':
                        with (run.ROOT/'.work/scale.lock').open('a') as lock:
                            fcntl.flock(lock,fcntl.LOCK_EX)
                            proof=native_case(cfg,dest,row[tool]['samples'][-1])
                    else:
                        proof=native_case(cfg,dest,row[tool]['samples'][-1])
                if (not complete(proof) and tool=='rustsys'
                        and did in ('ibex','vexriscv-verilog')):
                    if any(any(marker in p.read_text() for marker in
                            ('Networks are NOT EQUIVALENT.','Verification failed')) for p in
                            (dest/'abc-cec.stdout',dest/'export/abc-cec.stdout') if p.exists()):
                        raise ValueError('native counterexample requires investigation: '+str(dest))
                    proof=observed_case(cfg,directory/'composed'/did/lib/tool,observation_checker,arisca)
                if not (large_lec.complete(proof) or slice_lec.complete(proof) or observed_proof.complete(proof)):raise ValueError('fresh RTL event proof incomplete: '+str(dest))
                for path,pinned in proof['source_sha256'].items():
                    if run.digest(path)!=pinned:raise ValueError('native external proof input changed')
                if digest not in proof['source_sha256'].values():raise ValueError('proof omits the measured netlist')
            elif digest==old_sha:
                proof=copy.deepcopy(original)
                proof['reuse_binding']={'netlist_sha256':digest,'source_and_library_sha256_checked':True,
                    'all_three_repetitions_checked':True,'thread_control_identical':thread_control}
            else:
                if did=='openc910':raise ValueError('large netlist changed; fresh full-core proof required')
                reference=old_root/did/lib/f'{tool}-3'/'netlist.v'
                if run.digest(reference)!=old_sha:raise ValueError('certified reference netlist changed')
                dest=directory/'bridges'/did/lib/tool;cfg_path=dest.parent/(tool+'-bridge-config.json');dest.parent.mkdir(parents=True,exist_ok=True)
                bridge_cfg={**cfg,'reference_gate_netlist':True,'netlist':str(netlist),
                            'design':{**cfg['design'],'files':[str(reference)]}}
                run.save(cfg_path,bridge_cfg)
                cached=run.read_json(dest/'result.json') if (dest/'result.json').exists() else None
                if cached and not report.complete_external_proof(cached):raise ValueError('incomplete gate bridge retained for investigation: '+str(dest))
                if not cached:
                    print('Proving changed netlist:',did,lib,tool,flush=True)
                    unit=f'rustsys-scaling-bridge-{os.getpid()}.scope'
                    sample=run.execute(['systemd-run','--user','--scope','--quiet','--unit='+unit,
                        '-p','MemoryMax=4G','-p','MemorySwapMax=1G','taskset','-c','31',
                        sys.executable,str(Path(__file__).resolve()),'_bridge',str(cfg_path),str(dest)],
                        dest.parent,tool+'-bridge',seconds=3700,memory_gib=16,max_file_bytes=1024**3)
                    subprocess.run(['systemctl','--user','kill','--signal=KILL',unit],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
                    if sample['status']!='ok':raise ValueError('external gate bridge incomplete: '+str(dest))
                    cached=run.read_json(dest/'result.json')
                if not report.complete_external_proof(cached):raise ValueError('gate bridge did not prove all obligations')
                for p,h in cached['source_sha256'].items():
                    if run.digest(p)!=h:raise ValueError('gate bridge input changed')
                proof={'method':METHOD,'status':'ok','verdict':'proven','partitions':cached['partitions'],
                    'scope':{**result['scope'],'reset_assumptions':'none','transport':'certified RTL→old mapped netlist plus external old→measured mapped netlist event/state CEC'},
                    'certificate':{'original':original,'bridge':cached,'old_netlist_sha256':old_sha,'new_netlist_sha256':digest,
                        'library_sha256':prior['library_sha256'],'source_and_library_binding_checked':True},
                    'source_sha256':{**original_inputs,str(netlist):digest}}
            if not complete(proof):raise ValueError('final proof composition is incomplete')
            entry[tool]=proof
            print('Certified',did,lib,tool,digest,flush=True)
        result['rows'].append(entry)
        run.save(directory/'proofs.json',result)
    return result


def verify_timing(workspace,directory,data,sta,reuse=()):
    reusable=evidence_reuse.load(reuse,'timing')
    result={'schema':1,'reference':'OpenSTA opaque executable; no source inspection','rows':[]}
    for row in data['rows']:
        if row['allocated_physical_cores']!=16:continue
        entry={'id':row['id'],'design':row['design'],'library':row['library']}
        for tool in ('rustsys','yosys'):
            folder=workspace/row['id']/row['library']/'16c'/f'{tool}-3';cfg=run.read_json(folder/'config.json')
            dest=directory/'sta'/row['id']/row['library']/tool
            reused=evidence_reuse.timing(reusable,(row['id'],row['library'],tool),folder/'netlist.v',cfg['library'],sta,CLOCK[row['id']])
            if reused is not None:r=reused
            elif (dest/'result.json').exists():r=run.read_json(dest/'result.json')
            else:
                print('Timing',row['id'],row['library'],tool,flush=True)
                # Small cases need<2GiB RSS; full-core graph estimate16GiB.
                r=sta_benchmark.measure(folder/'netlist.v',cfg['library'],cfg['design']['top'],CLOCK[row['id']],sta,dest,
                                        memory_gib=32 if row['id']=='openc910' else 4,seconds=7200,cpu=31)
            if r['status']!='ok' and any('syntax error' in p.read_text() for p in (dest/'sta.stdout',dest/'sta.stderr') if p.exists()):
                # Keep the failed reader receipt intact. Only the checked
                # unused-wire declaration adapter can justify a format retry.
                retry=dest/'structural-format-retry'
                if (retry/'result.json').exists():r=run.read_json(retry/'result.json')
                else:r=sta_benchmark.measure(folder/'netlist.v',cfg['library'],cfg['design']['top'],CLOCK[row['id']],sta,retry,
                                             memory_gib=32 if row['id']=='openc910' else 4,seconds=7200,cpu=31)
            if r['status']!='ok':raise ValueError('OpenSTA analysis incomplete: '+str(dest))
            for p,h in r['sha256'].items():
                if run.digest(p)!=h:raise ValueError('timing input or constraint changed')
            entry[tool]=r
            print('Setup Fmax',row['id'],row['library'],tool,r['fmax_mhz'],flush=True)
        result['rows'].append(entry);run.save(directory/'timing.json',result)
    return result


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--workspace',type=Path,required=True);p.add_argument('--directory',type=Path,required=True)
    p.add_argument('--sta',type=Path,required=True);p.add_argument('--wait-seconds',type=int,default=86400)
    p.add_argument('--phase',choices=('proofs','timing','all'),default='all')
    p.add_argument('--observation-checker',type=Path,help='optional check_probe development driver for arithmetic observations')
    p.add_argument('--arisca',type=Path,help='opaque Arisca executable; default from pinned local tool manifest or PATH')
    p.add_argument('--reuse-proofs',type=Path,action='append',default=[],help='complete previous proof records, reusable only for exact RTL/library/netlist bytes')
    p.add_argument('--reuse-timing',type=Path,action='append',default=[],help='complete previous timing receipts for exact netlist/library bytes')
    a=p.parse_args()
    if not 1<=a.wait_seconds<=86400:p.error('invalid wait budget')
    directory=a.directory.resolve();directory.mkdir(parents=True,exist_ok=True)
    data=wait_ready(a.workspace.resolve(),a.wait_seconds)
    if a.phase in ('proofs','all'):verify_proofs(a.workspace.resolve(),directory,data,a.observation_checker,a.arisca,a.reuse_proofs)
    if a.phase in ('timing','all'):verify_timing(a.workspace.resolve(),directory,data,a.sta.resolve(),a.reuse_timing)
    run.save(directory/('complete-'+a.phase+'.json'),{'status':'complete','phase':a.phase})


if __name__=='__main__':
    try:
        if len(sys.argv)==4 and sys.argv[1]=='_bridge':raise SystemExit(bridge_worker(sys.argv[2],sys.argv[3]))
        main()
    except (ValueError,OSError,MemoryError) as error:
        print('BENCH_SCALING_VALIDATION_UNVERIFIED:',error,file=sys.stderr)
        raise SystemExit(2)
