#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Bounded large-design synthesis comparison using opaque reference tools.

Upfront plan for OpenC910 ct_core: 235k word cells, 1.45M memory bits observed
before memory lowering. Conservative 6M Boolean nodes * (256 B graph/state +
40 cuts * 48 B) = 12.2 GiB, plus <=3 GiB frontend/netlist scratch. One worker,
one corner, 16 GiB physical memory, 1 GiB swap, explicit wall/file/workspace
guards and an exclusive scale lock. No paths or cones are enumerated here.
Preparation is streaming Verilog macro expansion (<128 MiB output, <256 MiB
planning RAM), not an external synthesis/elaboration pass. Standard-cell RAM
expansion is inside each tool's timed region. Proofs are separate and never
inferred from synthesis completion or matching area/cell counts.
"""
from pathlib import Path
from datetime import datetime, timezone
import argparse
import copy
import fcntl
import os
import subprocess
import sys
import tomllib

import lec
import run

MANIFEST=run.read_json(run.HERE/'large-designs.json')


def prepare(args):
    spec=MANIFEST['design'];limits=MANIFEST['limits'];repo=args.source_root.resolve()
    head=subprocess.check_output(['git','-C',str(repo),'rev-parse','HEAD'],text=True).strip()
    if head!=spec['commit']:raise ValueError('hardware revision differs from pinned manifest')
    root=repo/'C910_RTL_FACTORY';filelist=repo/spec['filelist'];files=[]
    for line in filelist.read_text().splitlines():
        line=line.strip()
        if not line:continue
        if not line.startswith('${CODE_BASE_PATH}/'):raise ValueError('unsupported hardware file-list directive')
        path=(root/line.removeprefix('${CODE_BASE_PATH}/')).resolve()
        if not path.is_relative_to(repo) or not path.is_file():raise ValueError('invalid hardware source path')
        files.append(path)
        if len(files)>limits['max_source_files']:raise ValueError('source file-count budget reached')
    if sum(p.stat().st_size for p in files)>limits['max_source_bytes']:
        raise ValueError('source byte budget reached')
    d=args.workspace.resolve();d.mkdir(parents=True,exist_ok=False);target=d/'openc910.v'
    sources={str(p.relative_to(repo)):run.digest(p) for p in [*files,filelist,repo/'LICENSE']}
    r=run.execute([str(args.preprocessor.resolve()),'-E','-g2012','-DSYNTHESIS','-o',str(target),
                   *map(str,files)],d,'preprocess',seconds=120,memory_gib=2,
                   max_file_bytes=limits['max_prepared_bytes'])
    if r['status']!='ok':raise ValueError('Verilog preprocessing did not finish')
    if target.stat().st_size!=MANIFEST['prepared_bytes'] or run.digest(target)!=MANIFEST['prepared_sha256']:
        raise ValueError('prepared RTL differs from pinned preprocessing output')
    if sources!={name:run.digest(repo/name) for name in sources}:raise ValueError('hardware changed during preparation')
    design={**spec,'files':[str(target)],'sha256':{target.name:run.digest(target)},
            'source_files':sources,'preparation':r,'preprocessor':{
                'path':str(args.preprocessor.resolve()),'sha256':run.digest(args.preprocessor)}}
    run.save(d/'input.json',design);print('Prepared',len(files),'files;',target.stat().st_size,'bytes',flush=True)


def compare(args):
    design=run.read_json(args.prepared.resolve()/'input.json');d=args.workspace.resolve()
    if any(design.get(k)!=MANIFEST['design'][k] for k in ('id','top','commit','repo')):
        raise ValueError('prepared hardware identity differs from pinned manifest')
    d.mkdir(parents=True,exist_ok=False)
    inputs={p:run.digest(p) for p in design['files']}
    if any(inputs[p]!=design['sha256'][Path(p).name] for p in inputs):raise ValueError('prepared source changed')
    if len(inputs)!=1 or set(inputs.values())!={MANIFEST['prepared_sha256']}:
        raise ValueError('benchmark requires the exact complete pinned core')
    pdks={x['name']:x for x in tomllib.loads((run.ROOT/'verification/pdks.toml').read_text())['library']}
    tools={n:{'path':str(getattr(args,n).resolve()),'sha256':run.digest(getattr(args,n))}
           for n in ('rustsys','rusteda','yosys')}
    cpu=min(os.sched_getaffinity(0)) if args.cpu is None else args.cpu
    if cpu not in os.sched_getaffinity(0):raise ValueError('selected logical CPU is unavailable')
    if run.version(args.yosys,'-V')!=run.MANIFEST['reference_versions']['yosys']:
        raise ValueError('reference Yosys version differs from pinned profile')
    result={'schema':1,'date':datetime.now(timezone.utc).isoformat(),'design':design,'tools':tools,
            'limits':{'threads':1,'cpu_affinity':[cpu],'repetitions':args.repetitions,'warmups':args.warmups,'wall_s':args.timeout,
                      'physical_memory_gib':args.memory_gib,'swap_gib':1,**MANIFEST['limits']},
            'scope':{'corner':'per library','wire_model':'none','unconstrained_endpoints':'all; no SDC or STA',
                     'reset_assumptions':'none','drc_status':'unchecked','ram_model':'upstream functional RTL; standard-cell lowering included',
                     'yosys_resource_sharing':not args.yosys_noshare,
                     'repeat_determinism':'checked' if args.repetitions>1 else 'unchecked; single measured stress run'},
            'rows':[]}
    for name in MANIFEST['libraries']:
        pdk=pdks[name];lib=args.pdks.resolve()/f'{pdk["library"]}__{pdk["corner"]}.lib'
        if run.digest(lib)!=pdk['sha256']:raise ValueError('PDK checksum differs')
        row={'design':design['name'],'library':name,'corner':pdk['corner'],'library_sha256':pdk['sha256']}
        result['rows'].append(row);samples={t:[] for t in ('rustsys','yosys')};failed=set()
        for rep in range(0 if args.warmups else 1,args.repetitions+1):
            for tool in (('rustsys','yosys') if rep%2==0 else ('yosys','rustsys')):
                if tool in failed:continue
                folder=d/name/f'{tool}-{rep}';folder.mkdir(parents=True)
                cfg={'directory':str(folder),'tool':tool,'library':str(lib),'corner':pdk['corner'],'design':design,'quiet_yosys':True,'yosys_noshare':args.yosys_noshare,
                     **{t:v['path'] for t,v in tools.items()}}
                run.save(folder/'config.json',cfg)
                # A child-only cgroup protects this reporting parent if the
                # compiler exhausts memory; the measured timer includes launch.
                unit='rustsys-large-'+run.digest(folder/'config.json')[:16]+'.scope'
                command=['systemd-run','--user','--scope','--quiet','--unit='+unit,
                    '-p',f'MemoryMax={args.memory_gib}G','-p','MemorySwapMax=1G',
                    'taskset','-c',str(cpu),sys.executable,str(run.HERE/'run.py'),'_pipeline',str(folder/'config.json')]
                sample=run.execute(command,folder,'synth',seconds=args.timeout,memory_gib=args.memory_gib+8,
                    max_file_bytes=MANIFEST['limits']['max_output_bytes'])
                subprocess.run(['systemctl','--user','kill','--signal=KILL',unit],
                    stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
                if sample['status']=='ok':
                    sample['qor']=run.qos(folder,tool);sample['netlist_sha256']=run.digest(folder/'netlist.v')
                    if sample['qor'].get('unmapped'):sample['status']='unmapped_cells'
                print(name,tool,'warmup' if rep==0 else rep,sample['status'],sample['wall_s'],
                      sample['peak_rss_kib'],'KiB',flush=True)
                if rep or sample['status']!='ok':samples[tool].append(sample)
                if sample['status']!='ok':failed.add(tool)
                for t in samples:
                    if samples[t]:
                        row[t]=run.summarize(samples[t]);row[t]['equivalence']={'status':'not_run','verdict':'unverified'}
                        if row[t]['status']=='ok' and len(samples[t])!=args.repetitions:
                            row[t]['status']='incomplete'
                run.save(d/'results.json',result)
                lec.tree_budget(d,MANIFEST['limits']['max_workspace_bytes'],MANIFEST['limits']['max_workspace_files'])
                if any(run.digest(p)!=h for p,h in inputs.items()):raise ValueError('timed source changed')
        for t,info in tools.items():
            if run.digest(info['path'])!=info['sha256']:raise ValueError('benchmark executable changed')
    run.save(d/'results.json',result);return result


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=('prepare','run'))
    p.add_argument('--workspace',type=Path,required=True);p.add_argument('--source-root',type=Path)
    p.add_argument('--preprocessor',type=Path);p.add_argument('--prepared',type=Path)
    p.add_argument('--pdks',type=Path,default=run.ROOT/'target/pdk')
    for tool in ('rustsys','rusteda'):p.add_argument('--'+tool,type=Path,default=run.ROOT/'target/release'/tool)
    p.add_argument('--yosys',type=Path);p.add_argument('--timeout',type=int,default=3600)
    p.add_argument('--memory-gib',type=int,default=16);p.add_argument('--repetitions',type=int,choices=range(1,6),default=3)
    p.add_argument('--cpu',type=int,help='logical CPU used by every tool and child process')
    p.add_argument('--warmups',type=int,choices=(0,1),default=1)
    p.add_argument('--yosys-noshare',action='store_true',help='disable optional word-level SAT resource sharing; recorded as a distinct profile')
    a=p.parse_args()
    if a.workspace.exists():p.error('use a fresh workspace; old evidence is preserved')
    if a.action=='prepare' and (a.source_root is None or a.preprocessor is None):p.error('preparation requires source root and preprocessor')
    if a.action=='run' and (a.prepared is None or a.yosys is None):p.error('measurement requires prepared input and Yosys')
    if not 1<=a.memory_gib<=24 or not 1<=a.timeout<=14400:p.error('invalid resource budget')
    available=next(int(line.split()[1]) for line in Path('/proc/meminfo').read_text().splitlines() if line.startswith('MemAvailable:'))
    if available<(a.memory_gib+2)*1024**2:p.error('insufficient available host memory for selected guard')
    lock=run.ROOT/'.work/scale.lock';lock.parent.mkdir(parents=True,exist_ok=True)
    with lock.open('a') as stream:
        fcntl.flock(stream,fcntl.LOCK_EX)
        available=next(int(line.split()[1]) for line in Path('/proc/meminfo').read_text().splitlines() if line.startswith('MemAvailable:'))
        if available<(a.memory_gib+2)*1024**2:p.error('insufficient available host memory after waiting for the scale lock')
        try:prepare(a) if a.action=='prepare' else compare(a)
        except (ValueError,OSError,MemoryError) as error:
            a.workspace.mkdir(parents=True,exist_ok=True)
            run.save(a.workspace/'failure.json',{'status':'unverified','diagnostic':{'code':'BENCH_LARGE','message':str(error)}})
            raise SystemExit(2) from error
