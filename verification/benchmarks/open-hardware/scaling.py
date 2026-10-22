#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Serial, resource-bounded end-to-end CPU scaling measurements.

Upfront memory plan: OpenC910 has ~6M Boolean nodes plus a shared4GiB cut
budget, <=12GiB frontend/netlist state and <=32MiB scratch per worker (16
workers). Conservative20GiB RSS; children get32GiB RAM+1GiB swap,40GiB AS,
10800s and4GiB/file limits. Reporter holds <=64MiB JSON, one case at a time.
All >4GiB runs hold .work/scale.lock; no reference implementation is read.
"""
from pathlib import Path
import argparse
import copy
from datetime import datetime, timezone
import fcntl
import json
import math
import os
import shutil
import statistics
import subprocess
import sys
import time
import tomllib

import lec
import run


def available_kib():
    return next(int(s.split()[1]) for s in Path('/proc/meminfo').read_text().splitlines()
                if s.startswith('MemAvailable:'))


def atomic_save(path, value):
    temporary=path.with_suffix('.tmp')
    run.save(temporary,value)
    temporary.replace(path)


def configurations(inputs=None, large_input=None, pdks=None, yosys=None):
    """Construct commands from local pinned preparation manifests, not archived paths."""
    inputs=inputs or run.ROOT/'.work/open-hardware/inputs.json'
    large_input=large_input or run.ROOT/'.work/large-hardware-20261020/prepared-final/input.json'
    pdks=(pdks or run.ROOT/'target/pdk').resolve()
    yosys=(yosys or run.ROOT/'.work/open-hardware/yosys').resolve()
    designs=[*run.read_json(inputs),run.read_json(large_input)]
    specs={d['id']:d for d in run.MANIFEST['designs']}
    large_spec=run.read_json(run.HERE/'large-designs.json');specs['openc910']=large_spec['design']
    libraries={x['name']:x for x in tomllib.loads((run.ROOT/'verification/pdks.toml').read_text())['library']}
    result={}
    for design in designs:
        spec=specs.get(design['id'])
        if spec is None or any(design[key]!=spec[key] for key in ('top','commit','repo')):
            raise ValueError('prepared design identity differs from the pinned manifest')
        if design['id']=='openc910' and (len(design['files'])!=1 or run.digest(design['files'][0])!=large_spec['prepared_sha256']):
            raise ValueError('full-core preparation differs from the pinned complete RTL')
        for name in ('sky130hd_tt','gf180mcu7t_tt'):
            pdk=libraries[name];library=pdks/f"{pdk['library']}__{pdk['corner']}.lib"
            if run.digest(library)!=pdk['sha256']:raise ValueError('PDK checksum differs from pinned baseline')
            for tool in ('rustsys','yosys'):
                c={'design':design,'tool':tool,'library':str(library),'corner':pdk['corner'],
                   'yosys':str(yosys),'quiet_yosys':True}
                if design['id']=='openc910':
                    c.update(yosys_noshare=True)
                    if tool=='yosys':c.update(yosys_flow='arbitrary-initial-state-bounded-map-v1',yosys_bounded_map=True)
                result[(design['id'],name,tool)]=c
    return result


def aggregate(samples, expected):
    result=run.summarize(samples)
    if result['status']=='ok' and len(samples)!=expected:
        result['status']='incomplete'
    if result['status']=='ok':
        values=[s.get('average_cpu_cores') for s in samples]
        result['median_average_cpu_cores']=statistics.median(values) if all(v is not None for v in values) else None
        result['netlist_sha256']=samples[0]['netlist_sha256']
    return result


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--workspace',type=Path,required=True)
    p.add_argument('--cores',type=int,nargs='+',default=[1,16])
    p.add_argument('--design',action='append')
    p.add_argument('--library',action='append')
    p.add_argument('--tool',action='append',choices=('rustsys','yosys'))
    p.add_argument('--goal',choices=('area','delay'),default='area',help='RustSys mapping objective, recorded for every case')
    p.add_argument('--effort',choices=('low','medium','high'),default='medium')
    p.add_argument('--timing-period',type=float,help='enable RustSys load-aware refinement with this ideal-clock period in ns')
    p.add_argument('--area-limit',type=float,help='explicit RustSys area target; requires --timing-period')
    p.add_argument('--buffer-fanout',type=int,default=8,help='RustSys electrical buffer branch target (2..128)')
    p.add_argument('--profile-commands',action='store_true',help='record complete init/import/synth/export wall and CPU times inside the measured job')
    p.add_argument('--reference-cores',type=int,nargs='+',help='CPU allocations to measure for Yosys; RustSys still measures every --cores value')
    p.add_argument('--reference-initial-state-flow',action='store_true',help='use the same initial-state-preserving Yosys flow for the four smaller designs')
    p.add_argument('--reference-full-map',action='store_true',help='use initial-state-preserving flow and default ABC library mapping for every selected design, including OpenC910')
    p.add_argument('--reference-timing',action='store_true',help='use public constrained ABC mapping with buffering/sizing atop the arbitrary-state v3 flow')
    p.add_argument('--reference-period',type=float,default=1000.0,help='constrained ABC delay target in ns')
    p.add_argument('--reference-load-ff',type=float,default=10.0,help='constrained ABC output load in femtofarads')
    p.add_argument('--resume',action='store_true',help='resume only complete, hash-checked samples from this identical measurement configuration')
    p.add_argument('--repetitions',type=int,choices=range(1,6),default=3)
    p.add_argument('--warmups',type=int,choices=(0,1),default=1)
    p.add_argument('--memory-gib',type=int,default=32)
    p.add_argument('--timeout',type=int,default=10800)
    p.add_argument('--memory-wait-seconds',type=int,default=1800,help='bounded wait for host RAM before a measured child starts')
    p.add_argument('--binaries',type=Path,required=True)
    p.add_argument('--inputs',type=Path,default=run.ROOT/'.work/open-hardware/inputs.json')
    p.add_argument('--large-input',type=Path,default=run.ROOT/'.work/large-hardware-20261020/prepared-final/input.json')
    p.add_argument('--pdks',type=Path,default=run.ROOT/'target/pdk')
    p.add_argument('--yosys',type=Path,default=run.ROOT/'.work/open-hardware/yosys')
    p.add_argument('--reference-executables',type=Path,help='optional manifest of actual reference executables and pinned hashes behind launchers')
    a=p.parse_args()
    if not 8<=a.memory_gib<=32 or not 1<=a.timeout<=14400 or not 0<=a.memory_wait_seconds<=3600:p.error('invalid resource budget')
    if any(x is not None and (not math.isfinite(x) or x<=0) for x in (a.timing_period,a.area_limit)):
        p.error('timing and area targets must be finite and positive')
    if a.area_limit is not None and a.timing_period is None:p.error('--area-limit requires --timing-period')
    if not 2<=a.buffer_fanout<=128:p.error('invalid buffer fanout target')
    if a.buffer_fanout!=8 and a.timing_period is None:p.error('--buffer-fanout requires --timing-period')
    if a.reference_timing:
        if (not math.isfinite(a.reference_period*1000) or a.reference_period<=0
                or not math.isfinite(a.reference_load_ff) or a.reference_load_ff<0):p.error('invalid reference timing constraint')
        a.reference_full_map=True
    profile=(a.goal!='area' or a.effort!='medium' or a.timing_period is not None or a.profile_commands)
    # One hardware thread per physical core; SMT is not counted as extra cores.
    physical={}
    for cpu in sorted(os.sched_getaffinity(0)):
        topology=Path(f'/sys/devices/system/cpu/cpu{cpu}/topology')
        key=((topology/'physical_package_id').read_text().strip(),(topology/'core_id').read_text().strip())
        physical.setdefault(key,cpu)
    cpus=list(physical.values())
    if not a.cores or len(set(a.cores))!=len(a.cores) or any(c<1 or c>len(cpus) for c in a.cores):p.error('invalid physical CPU count')
    if a.reference_cores and not set(a.reference_cores)<=set(a.cores):p.error('reference CPU counts must be in --cores')
    all_configs=configurations(a.inputs,a.large_input,a.pdks,a.yosys)
    rustsys_profile={'rustsys_goal':a.goal,'rustsys_effort':a.effort,'timing_period':a.timing_period,
                     'area_limit':a.area_limit,'buffer_fanout':a.buffer_fanout,'profile_commands':a.profile_commands}
    if profile:
        for (_,_,tool),cfg in all_configs.items():
            if tool=='rustsys':cfg.update(rustsys_flow='workspace-profile-v1',**rustsys_profile)
    if a.reference_initial_state_flow:
        for (did,_,tool),cfg in all_configs.items():
            if tool=='yosys' and did!='openc910':
                cfg.update(yosys_flow='arbitrary-initial-state-full-map-v1',yosys_bounded_map=False,
                           yosys_noshare=True,normalize_case_promises=True)
    if a.reference_full_map:
        for (_,_,tool),cfg in all_configs.items():
            if tool=='yosys':
                cfg.update(yosys_flow='arbitrary-initial-state-full-map-v3',yosys_bounded_map=False,
                           yosys_delay_ff_optimization=True,
                           yosys_abc_fast=False,yosys_noshare=True,normalize_case_promises=True)
    if a.reference_timing:
        drivers={'sky130hd_tt':'sky130_fd_sc_hd__buf_1','gf180mcu7t_tt':'gf180mcu_fd_sc_mcu7t5v0__buf_1'}
        for (_,library,tool),cfg in all_configs.items():
            if tool=='yosys':cfg.update(yosys_timing={'driving_cell':drivers[library],
                'output_load_ff':a.reference_load_ff,'period_ns':a.reference_period})
    selected=[(key,c) for key,c in all_configs.items()
              if (not a.design or key[0] in a.design) and (not a.library or key[1] in a.library)
              and (not a.tool or key[2] in a.tool)]
    if not selected:p.error('no matching measured cases')
    out=a.workspace.resolve()
    if a.resume:
        data=run.read_json(out/'results.json')
    else:
        out.mkdir(parents=True,exist_ok=False)
    source_pins={p:run.digest(p) for _,c in selected for p in [c['library'],*c['design']['files']]}
    for _,c in selected:
        if any(run.digest(f)!=c['design']['sha256'][Path(f).name] for f in c['design']['files']):raise ValueError('prepared RTL changed')
    binaries={name:a.binaries.resolve()/name for name in ('rustsys','rusteda')}
    binary_pins={str(p):run.digest(p) for p in binaries.values()}
    ref_paths={str(Path(c['yosys']).resolve()) for _,c in selected}
    binary_pins.update({name:run.digest(name) for name in ref_paths})
    snapshot=out/'harness'
    if not a.resume:
        snapshot.mkdir()
        for path in run.HERE.iterdir():
            if path.is_file() and path.suffix in ('.py','.json','.v'):shutil.copy2(path,snapshot/path.name)
    harness_pins={p.name:run.digest(p) for p in snapshot.iterdir() if p.is_file()}
    fresh={'schema':1,'date':datetime.now(timezone.utc).isoformat(),'limits':{
        'repetitions':a.repetitions,'warmups':a.warmups,'physical_cores':a.cores,
        'cpu_pool':cpus,'physical_memory_gib':a.memory_gib,'swap_gib':1,'wall_s':a.timeout,
        'max_workspace_bytes':256*1024**3,'max_workspace_files':200000},
        'scope':{'timer':'prepared RTL through disk-workspace init/import, synthesis, mmap and full Verilog export; no preparation/proofs/STA',
            'corner':'per library','wire_model':'none','timing_status':'not_analyzed_in_synthesis_measurement',
            'unconstrained_endpoints':'all during synthesis; separate STA receipts required',
            'drc_status':'unchecked','reset_assumptions':'none',
            'qor_comparison':'identical input/library/effort for each thread count; unchanged netlist hashes required',
            'yosys_openc910_profile':'arbitrary-initial-state-bounded-map-v1; same as verified baseline',
            'other_yosys_profiles':'original four-core standard synth/ABC profile',
            'sampling':'all warmups excluded; three measured runs by default; CPU count is allocation, not a claim of utilization'},
        'binary_sha256':binary_pins,'input_sha256':source_pins,'harness_sha256':harness_pins,'rows':[]}
    if profile:fresh['limits']['rustsys_profile']=rustsys_profile
    if (any(key[2]=='yosys' for key,_ in selected) and {1,16}<=set(a.cores)
            and {1,16}<=set(a.reference_cores or a.cores)):
        fresh['scope']['required_reference_cores']=[1,16]
    if a.reference_timing:fresh['limits']['yosys_timing']={'period_ns':a.reference_period,'output_load_ff':a.reference_load_ff,'drivers':drivers}
    if a.timing_period is not None:
        fresh['scope'].update(
            timing_status='RustSys native load-aware electrical estimate; independent OpenSTA validation remains separate',
            unconstrained_endpoints='native electrical report scope and separate OpenSTA constraint receipts; not presumed zero',
            native_electrical_period_ns=a.timing_period,
            native_electrical_area_target=a.area_limit)
    if a.resume:
        for key in ('limits','binary_sha256','input_sha256','harness_sha256'):
            if data[key]!=fresh[key]:raise ValueError('resume configuration or pinned bytes differ: '+key)
        data.setdefault('resumes',[]).append({'date':datetime.now(timezone.utc).isoformat(),
            'controller_sha256':run.digest(Path(__file__))})
    else:data=fresh
    if a.reference_initial_state_flow:
        data['scope']['other_yosys_profiles']='initial-state-preserving structural memory lowering; data completion before FF optimization; -keepdc; default ABC library mapping'
    if a.reference_full_map:
        data['scope']['other_yosys_profiles']='initial-state-preserving structural memory lowering; -keepdc; zero data completion; default ABC library mapping'
        data['scope']['yosys_openc910_profile']='same initial-state-preserving flow and default ABC library mapping as every other design'
    if a.reference_timing:
        data['scope']['other_yosys_profiles']='arbitrary-state v3 plus public constrained default ABC buffer/upsize/dnsize; no FFs passed to ABC'
        data['scope']['yosys_openc910_profile']=data['scope']['other_yosys_profiles']
    data['scheduled_reference_cores']=a.reference_cores or a.cores
    data['memory_wait_seconds']=a.memory_wait_seconds
    manifest=a.reference_executables or run.ROOT/'.work/large-hardware-20261020/reference-executables.json'
    references=run.read_json(manifest) if manifest.exists() else [{'path':str(a.yosys.resolve()),'sha256':run.digest(a.yosys)}]
    reference_pins={r['path']:r['sha256'] for r in references}
    if any(run.digest(path)!=digest for path,digest in reference_pins.items()):raise ValueError('opaque reference executable changed')
    if run.version(next(iter(ref_paths)),'-V')!=run.MANIFEST['reference_versions']['yosys']:raise ValueError('Yosys version differs from pinned profile')
    data['reference_executables']=references
    data['reference_version']=run.MANIFEST['reference_versions']['yosys']
    atomic_save(out/'results.json',data)
    lock=run.ROOT/'.work/scale.lock'
    with lock.open('a') as stream:
        fcntl.flock(stream,fcntl.LOCK_EX)
        for did in dict.fromkeys(key[0] for key,_ in selected):
            for library in dict.fromkeys(key[1] for key,_ in selected if key[0]==did):
                for cores in a.cores:
                    row=next((r for r in data['rows'] if (r['id'],r['library'],r['allocated_physical_cores'])==(did,library,cores)),None)
                    if row is None:
                        row={'id':did,'design':next(c['design']['name'] for key,c in selected if key[0]==did),
                             'library':library,'allocated_physical_cores':cores,'cpu_affinity':cpus[:cores]}
                        data['rows'].append(row)
                    samples={t:[] for t in ('rustsys','yosys') if (did,library,t) in all_configs and (not a.tool or t in a.tool)
                             and (t!='yosys' or not a.reference_cores or cores in a.reference_cores)}
                    for rep in range(0 if a.warmups else 1,a.repetitions+1):
                        for tool in (('rustsys','yosys') if rep%2==0 else ('yosys','rustsys')):
                            if tool not in samples:continue
                            folder=out/did/library/f'{cores}c'/f'{tool}-{rep}'
                            if a.resume and (folder/'sample.json').exists():
                                sample=run.read_json(folder/'sample.json')
                                c=run.read_json(folder/'config.json')
                                if (sample['status']!='ok' or run.digest(folder/'netlist.v')!=sample.get('netlist_sha256')
                                        or c.get('threads')!=cores or c['design']['id']!=did or c['tool']!=tool):
                                    raise ValueError('resume sample is incomplete or changed: '+str(folder))
                                if rep:samples[tool].append(sample)
                                row[tool]=aggregate(samples[tool],a.repetitions) if samples[tool] else {'status':'warming_up','samples':[]}
                                row[tool]['equivalence']={'status':'not_run','verdict':'unverified'}
                                atomic_save(out/'results.json',data)
                                continue
                            deadline=time.monotonic()+a.memory_wait_seconds
                            waiting=False
                            while available_kib()<(a.memory_gib+2)*1024**2:
                                if not waiting:print('Waiting for available host RAM before starting the child',flush=True)
                                waiting=True
                                if time.monotonic()>=deadline:raise ValueError('insufficient available host RAM for the physical-memory guard')
                                time.sleep(min(5,deadline-time.monotonic()))
                            if folder.exists():
                                # Preserve an interrupted attempt; never accept it as a sample.
                                folder.rename(folder.with_name(folder.name+'-interrupted-'+str(time.time_ns())))
                            folder.mkdir(parents=True)
                            c=copy.deepcopy(all_configs[(did,library,tool)])
                            c.update(directory=str(folder),threads=cores,**{k:str(v) for k,v in binaries.items()})
                            run.save(folder/'config.json',c)
                            unit=f'rustsys-scaling-{os.getpid()}-{did}-{library}-{cores}-{tool}-{rep}.scope'
                            if tool=='yosys' and c.get('yosys_timing'):
                                child=[sys.executable,str(snapshot/'yosys_timing.py'),str(folder/'config.json')]
                            elif tool=='yosys' and c.get('yosys_flow'):
                                child=[sys.executable,str(snapshot/'yosys_initial.py'),str(folder/'config.json')]
                            elif tool=='rustsys' and c.get('rustsys_flow'):
                                child=[sys.executable,str(snapshot/'rustsys_profile.py'),str(folder/'config.json')]
                            else:child=[sys.executable,str(snapshot/'run.py'),'_pipeline',str(folder/'config.json')]
                            telemetry={}
                            def monitor():
                                lec.tree_budget(out,max_bytes=256*1024**3,max_files=200000)
                                if not telemetry.get('cgroup'):
                                    props=subprocess.run(['systemctl','--user','show',unit,'-p','ControlGroup','--value'],capture_output=True,text=True,timeout=5)
                                    if props.returncode==0 and props.stdout.strip():telemetry['cgroup']=props.stdout.strip()
                                if telemetry.get('cgroup'):
                                    group=Path('/sys/fs/cgroup')/telemetry['cgroup'].lstrip('/')
                                    for name in ('memory.peak','memory.swap.peak','cpu.stat'):
                                        try:telemetry[name]=(group/name).read_text().strip()
                                        except FileNotFoundError:pass
                            print(did,library,f'{cores}c',tool,'warmup' if rep==0 else rep,'starting',flush=True)
                            try:
                                sample=run.execute(['systemd-run','--user','--scope','--quiet','--unit='+unit,
                                    '-p',f'MemoryMax={a.memory_gib}G','-p','MemorySwapMax=1G',
                                    'taskset','-c',','.join(map(str,cpus[:cores])),*child],folder,'synth',
                                    seconds=a.timeout,memory_gib=a.memory_gib+8,max_file_bytes=4*1024**3,
                                    worker_threads=cores,monitor=monitor)
                            finally:subprocess.run(['systemctl','--user','kill','--signal=KILL',unit],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
                            sample['cgroup_telemetry']=telemetry
                            if sample['status']=='ok':
                                sample.update(qor=run.qos(folder,tool),netlist_sha256=run.digest(folder/'netlist.v'))
                                if sample['qor'].get('unmapped'):sample['status']='unmapped_cells'
                            run.save(folder/'sample.json',sample)
                            print(did,library,f'{cores}c',tool,rep,sample['status'],sample['wall_s'],sample.get('average_cpu_cores'),flush=True)
                            if rep:samples[tool].append(sample)
                            row[tool]=aggregate(samples[tool],a.repetitions) if samples[tool] else {'status':'warming_up','samples':[]}
                            row[tool]['equivalence']={'status':'not_run','verdict':'unverified'}
                            atomic_save(out/'results.json',data)
                            if sample['status']!='ok':raise ValueError(f'synthesis incomplete: {folder}')
                            # The source/producer bytes must stay pinned across all repetitions.
                            if any(run.digest(f)!=h for f,h in {**source_pins,**binary_pins,**reference_pins}.items()):raise ValueError('measured input or executable changed')
    atomic_save(out/'results.json',data)
    print('Measurement campaign completed:',out,flush=True)


if __name__=='__main__':
    try:main()
    except (ValueError,OSError,MemoryError) as error:
        print('BENCH_SCALING_UNVERIFIED:',error,file=sys.stderr)
        raise SystemExit(2)
