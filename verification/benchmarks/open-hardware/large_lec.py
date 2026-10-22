#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Independent RTL event-model obligations checked by an opaque ABC CEC binary.

Memory plan: <=720 MiB mapped gate tables, ~35M definitions *16 B, bounded
state/name/obligation tables and <=64M AIG ANDs *8 B per side. Model export and
ABC run serially in separate configurable physical-memory scopes (8 GiB default).
Large text imports may use 24 GiB: ~1.3 GiB text, <=100M token/AST entries
at ~128 B, <=5M cells/25M connections and frozen tables, one corner/worker.
One worker, one
corner, an exclusive scale lock, 2 GiB per file, <=8 GiB of proof artifacts.
Native X/Z-as-zero semantics are stated explicitly; no reset/input assumption
or choice of implementation initial state is permitted. Export is not proof.
"""
from pathlib import Path
from array import array
import argparse
import copy
import fcntl
import hashlib
import os
import json
import subprocess
import sys
import time

import lec
import run
import transition

METHOD='abc-independent-rtl-event-obligations-v1'
HARNESS_PINS={name:run.digest(run.HERE/name) for name in
              ('large_lec.py','run.py','transition.py','lec.py','equiv.py','rtl_semantics.py')}


def complete(proof):
    c=proof.get('certificate',{});m=c.get('manifest',{});s=m.get('sides',[])
    k=m.get('obligation_kinds',{});n=m.get('gate_state_elements',0)
    return (proof.get('method')==METHOD and proof.get('status')=='ok'
        and proof.get('verdict')=='proven' and proof.get('partitions')==1
        and c.get('equivalent') is True and c.get('artifact_hashes_checked') is True
        and c.get('interface_header_checked') is True
        and c.get('gate_state_inventory_checked',c.get('producer_state_count_checked')) is True
        and c.get('state_relation_coverage_checked') is True
        and m.get('all_rtl_inputs_and_states_quantified') is True
        and m.get('all_gate_initial_states_covered') is True
        and m.get('state_pairs')==n and type(n) is int and n>0
        and m.get('reset_assumptions')==[] and m.get('initial_state_choices')==[]
        and m.get('case_assumptions')==[] and proof.get('scope',{}).get('reset_assumptions')=='none'
        and k.get('output',0)==m.get('output_bits') and k.get('output',0)>0
        and k.get('clock',0)+k.get('enable',0)==n
        and k.get('async-clear',0)==n and k.get('async-preset',0)==n
        and k.get('next-state',0)+k.get('data',0)==n
        and len(s)==2 and s[0].get('inputs')==s[1].get('inputs')
        and s[0].get('inputs')==m.get('primary_input_bits',0)+m.get('rtl_state_bits',0)
        and all(side.get('outputs')==sum(k.values()) for side in s)
        and c.get('abc',{}).get('status')=='ok'
        and len(c.get('gold_sha256',''))==64 and len(c.get('gate_sha256',''))==64)


def verify_relation(directory,manifest):
    n=manifest['gate_state_elements'];nr=manifest['rtl_state_bits']
    inputs=manifest['sides'][0]['inputs'];variables=directory/'input-variables.u32le'
    if variables.stat().st_size!=4*inputs or inputs!=manifest['primary_input_bits']+nr:
        raise ValueError('state/input interface is incomplete')
    ids=array('I')
    with variables.open('rb') as f:ids.fromfile(f,inputs)
    if sys.byteorder!='little':ids.byteswap()
    maximum=max(ids,default=0)
    if maximum>=128_000_000:raise ValueError('input variable index budget reached')
    present=bytearray(maximum+1)
    for v in ids:
        if present[v]:raise ValueError('duplicate CEC input variable')
        present[v]=1
    relation=directory/'state-relation.jsonl'
    if relation.stat().st_size>2*1024**3:raise ValueError('relation file budget reached')
    seen_gate=bytearray(n);seen_rtl=bytearray(nr);records=0
    with relation.open() as f:
        while line:=f.readline(65537):
            if len(line)>65536:raise ValueError('relation record byte budget reached')
            entry=json.loads(line);g=entry['gate_index'];r=entry['rtl_index'];v=entry['rtl_variable']
            if (records>=n or type(g)is not int or type(r)is not int or type(v)is not int
                    or not 0<=g<n or not 0<=r<nr or not 0<=v<len(present)
                    or not present[v] or seen_gate[g] or seen_rtl[r]
                    or type(entry['inverted'])is not bool or not entry['rtl_name'] or not entry['gate_instance']):
                raise ValueError('invalid, duplicated or unbound state relation')
            seen_gate[g]=1;seen_rtl[r]=1;records+=1
    if records!=n or records!=manifest['state_relation_records'] or not all(seen_gate):
        raise ValueError('state relation omits implementation elements')
    return {'state_relation_sha256':run.digest(relation),'input_variables_sha256':run.digest(variables)}


def verify_inventory(manifest,producer,require_full_core):
    """The implementation inventory must match; independent RTL allocations may differ.

    The native interpreter may retain additional unused RTL declaration bits.
    They remain universally quantified. Equality with a synthesis frontend's
    pre-pruning allocation is not a proof precondition for a generic design.
    """
    if manifest['gate_state_elements']!=producer['state_bits_retained']:
        raise ValueError('independent model and producer retained-state inventories differ')
    if require_full_core and manifest['rtl_state_bits']!=producer['state_bits_rtl']:
        raise ValueError('full-core independent RTL and producer inventories differ')
    return {'independent_rtl_state_bits':manifest['rtl_state_bits'],
            'producer_rtl_state_bits':producer['state_bits_rtl'],
            'implementation_state_elements':manifest['gate_state_elements'],
            'retained_state_count_checked':True,
            'rtl_allocation_count_equal':manifest['rtl_state_bits']==producer['state_bits_rtl'],
            'rtl_coverage':'all interpreter RTL state bits remain universally quantified'}


def prove(cfg,d,args,sample):
    d.mkdir(parents=True,exist_ok=False);start=time.monotonic();steps=[]
    memory_gib=getattr(args,'memory_gib',8);swap_gib=getattr(args,'swap_gib',1)
    export_only=getattr(args,'export_only',False)
    reimport=getattr(args,'reimport_netlists',False)
    require_full_core=getattr(args,'require_full_core',True)
    if not 4<=memory_gib<=32 or not 0<=swap_gib<=4:
        raise ValueError('invalid physical memory/swap guard')
    files=[*map(Path,cfg['design']['files']),Path(cfg['library'])]
    netlist=Path(cfg['directory'])/'netlist.v';files.append(netlist)
    source_hashes={};tools={}
    result={'method':METHOD,'status':'unverified','verdict':'unverified','partitions':0,
        'source_sha256':source_hashes,'tools':tools,'steps':steps,'harness_sha256':HARNESS_PINS,
        'limits':{'memory_gib':memory_gib,'swap_gib':swap_gib,'model_wall_s':args.model_seconds,
                  'cec_wall_s':args.seconds,'max_file_bytes':2*1024**3,'max_workspace_bytes':8*1024**3},
        'scope':{'reference':'independent RustSys checker RTL interpreter',
                 'rtl_unknown_literals':'X/Z-as-zero; distinct from four-state equality',
                 'reset_assumptions':'none','input_assumptions':'none',
                 'initial_states':'all implementation initial states under checked named correspondence',
                 'outputs':'all original output bits and all retained state clock/enable, clear, preset and next-data obligations',
                 'wire_model':'none','corner':cfg['corner'],'drc_status':'unchecked'}}

    def save(message=None):
        if message:result['diagnostic']={'code':'BENCH_LARGE_LEC','message':message}
        result['wall_s']=time.monotonic()-start;run.save(d/'result.json',result)
        return result

    def execute(argv,name,seconds):
        unit='rustsys-large-lec-'+hashlib.sha256((str(d)+name).encode()).hexdigest()[:16]+'.scope'
        command=['systemd-run','--user','--scope','--quiet','--unit='+unit,
                 '-p',f'MemoryMax={memory_gib}G','-p',f'MemorySwapMax={swap_gib}G','taskset','-c',str(args.cpu),*map(str,argv)]
        try:
            value=run.execute(command,d,name,seconds=seconds,memory_gib=memory_gib+swap_gib+4,
                          max_file_bytes=2*1024**3,
                          monitor=lambda:lec.tree_budget(d,max_bytes=8*1024**3,max_files=20000))
            props=subprocess.run(['systemctl','--user','show',unit,'-p','Result','-p','MemoryPeak','-p','MemorySwapPeak'],capture_output=True,text=True)
            value['scope_telemetry']={line.split('=',1)[0]:line.split('=',1)[1] for line in props.stdout.splitlines() if '=' in line}
        finally:
            subprocess.run(['systemctl','--user','kill','--signal=KILL',unit],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        if value['status']!='ok':
            value['collector_peak_rss_kib']=value.pop('peak_rss_kib',None)
            value['peak_rss_kib']=None
            value['peak_rss_status']='unavailable on failed scope; collector usage is not compiler/solver RSS'
        steps.append({'phase':name,**value});return value

    try:
        source_hashes.update({str(p):run.digest(p) for p in files})
        tools.update({name:{'path':str(path.resolve()),'sha256':run.digest(path)}
           for name,path in [('checker',args.checker),('abc',args.abc),('rusteda',args.rusteda)]})
        tools['abc']['version']=subprocess.check_output([args.abc,'-c','version'],text=True,timeout=10).strip()
        if run.digest(netlist)!=sample['netlist_sha256']:raise ValueError('measured netlist hash differs')
        if require_full_core:
            specification=run.read_json(run.HERE/'large-designs.json')
            if (cfg['design']['top']!=specification['design']['top']
                    or len(cfg['design']['files'])!=1
                    or run.digest(cfg['design']['files'][0])!=specification['prepared_sha256']):
                raise ValueError('LEC requires the exact complete pinned core')
        else:
            spec=next((item for item in run.MANIFEST['designs'] if item['id']==cfg['design']['id']),None)
            if spec is None or any(cfg['design'][key]!=spec[key] for key in ('top','commit','repo')):
                raise ValueError('LEC requires a pinned four-core design configuration')
            if any(run.digest(source)!=cfg['design']['sha256'][Path(source).name] for source in cfg['design']['files']):
                raise ValueError('prepared benchmark RTL changed')
        cfg=copy.deepcopy(cfg);cfg['netlist']=str(netlist)
        if cfg['tool']=='rustsys' and not reimport:
            cfg['frozen_workspace']=str(Path(cfg['directory'])/'ws')
            table=Path(cfg['frozen_workspace'])/'revisions/rev0002/netlist.rbin'
            source_hashes[str(table)]=run.digest(table)
        else:
            ws=d/'imported-workspace'
            for name,command in [('init',['init']),('import',['import','--base-rev','rev0','--lib',cfg['library'],'--corner',cfg['corner'],'--netlist',str(netlist)])]:
                value=execute([args.rusteda,'--workspace',ws,'--format','json',*command],name,args.model_seconds)
                if value['status']!='ok':return save('bounded gate-table import did not finish')
            cfg['frozen_workspace']=str(ws)
            table=ws/'revisions/rev0001/netlist.rbin'
            source_hashes[str(table)]=run.digest(table)
            result['scope']['gate_input']='measured exported Verilog reimported into fresh checked gate tables'
        cfg['export_obligations']=str(d/'obligations');run.save(d/'config.json',cfg)
        exported=execute([args.checker,d/'config.json',d/'native.json'],'model-export',args.model_seconds)
        if exported['status']!='ok':return save('bounded independent model/obligation export did not finish')
        if not (d/'obligations/manifest.json').exists():
            native=run.read_json(d/'native.json');result['native']=native
            return save('native model or state correspondence did not establish complete export preconditions')
        manifest=run.read_json(d/'obligations/manifest.json')
        if not manifest['all_gate_initial_states_covered'] or manifest['reset_assumptions'] or manifest['initial_state_choices'] or manifest['case_assumptions']:
            raise ValueError('incomplete or restricted initial-state relation')
        if require_full_core and (manifest['rtl_state_bits']<1_400_000 or manifest['state_pairs']<1_400_000):
            raise ValueError('export omitted the complete core state scale')
        inventory=None
        if cfg['tool']=='rustsys':
            producer_path=Path(cfg['directory'])/'report.json'
            producer=run.read_json(producer_path)
            source_hashes[str(producer_path)]=run.digest(producer_path)
            inventory=verify_inventory(manifest,producer,require_full_core)
        relation_hashes=verify_relation(d/'obligations',manifest)
        paths=[d/'obligations'/name for name in ('gold.aig','gate.aig')]
        for path,side in zip(paths,manifest['sides'],strict=True):
            with path.open('rb') as stream:header=stream.readline(256).decode().strip().split()
            if len(header)!=6 or header[0]!='aig':raise ValueError('invalid AIGER header')
            maximum,inputs,latches,outputs,ands=map(int,header[1:])
            if (latches!=0 or maximum!=inputs+ands or inputs!=side['inputs']
                    or outputs!=side['outputs'] or ands!=side['and_nodes']
                    or outputs!=sum(manifest['obligation_kinds'].values())):
                raise ValueError('AIGER interface/coverage differs from manifest')
        hashes=[run.digest(path) for path in paths]
        checked={'status':'not_run','reason':'export-only; exact sliced external CEC required'}
        equivalent=False
        if not export_only:
            command=transition.cec_command(transition.Limits(),args.seconds,*paths)
            checked=execute([args.abc,'-c',command],'abc-cec',args.seconds+5)
            equivalent=checked['status']=='ok' and transition.cec_equivalent((d/'abc-cec.stdout').read_text())
        unchanged=(all(run.digest(Path(p))==h for p,h in source_hashes.items())
                   and all(run.digest(Path(t['path']))==t['sha256'] for t in tools.values())
                   and hashes==[run.digest(path) for path in paths])
        unchanged=unchanged and relation_hashes==verify_relation(d/'obligations',manifest)
        unchanged=unchanged and HARNESS_PINS=={name:run.digest(run.HERE/name) for name in HARNESS_PINS}
        if not unchanged:raise ValueError('proof source, tool or obligation artifact changed')
        result['certificate']={'manifest':manifest,'manifest_sha256':run.digest(d/'obligations/manifest.json'),
            'gold_sha256':hashes[0],'gate_sha256':hashes[1],'equivalent':equivalent,
            'artifact_hashes_checked':unchanged,'interface_header_checked':True,
            'gate_state_inventory_checked':True,
            'producer_state_count_checked':True if cfg['tool']=='rustsys' else None,
            'state_inventory':inventory,
            'producer_state_count_status':'checked against synthesis report' if cfg['tool']=='rustsys' else 'not available; complete imported gate-state inventory checked independently',
            'state_relation_coverage_checked':True,
            **relation_hashes,'abc':checked}
        if equivalent:
            result.update(status='ok',verdict='proven',partitions=1)
            if not complete(result):raise ValueError('incomplete external CEC certificate')
            return save()
        if export_only:return save('complete checked model export; external sliced CEC still required')
        return save('external ABC did not prove all event/output obligations')
    except (OSError,ValueError,MemoryError) as error:
        result.update(status='unverified',verdict='unverified',partitions=0)
        return save(str(error))


def main(args):
    # Check RAM after acquiring the same lock held by synthesis measurements.
    lock=run.ROOT/'.work/scale.lock'
    with lock.open('a') as stream:
        fcntl.flock(stream,fcntl.LOCK_EX)
        available=next(int(line.split()[1]) for line in Path('/proc/meminfo').read_text().splitlines() if line.startswith('MemAvailable:'))
        if available<(args.memory_gib+2)*1024**2:raise ValueError('not enough available host RAM for requested proof guard')
        original=run.read_json(args.measurements/'results.json');combined=copy.deepcopy(original)
        if len(original['rows'])!=2 or (args.measurements/'failure.json').exists():raise ValueError('measurement is not complete')
        args.workspace.mkdir(exist_ok=False)
        for row in combined['rows']:
            for tool in ('rustsys','yosys'):
                old=row[tool];d=args.workspace/row['library']/tool;d.parent.mkdir(exist_ok=True)
                if old['status']!='ok':
                    old['equivalence']={'method':METHOD,'status':'not_run','verdict':'unverified',
                        'diagnostic':{'code':'BENCH_NO_NETLIST','message':'synthesis did not produce a complete measured netlist'}}
                else:
                    cfg=run.read_json(args.measurements/row['library']/(tool+'-1')/'config.json')
                    if run.digest(cfg['library'])!=row['library_sha256']:raise ValueError('measured PDK changed')
                    old['equivalence']=prove(cfg,d,args,old['samples'][0])
                    print(row['library'],tool,old['equivalence']['verdict'],flush=True)
                run.save(args.workspace/'combined-results.json',combined)
        return combined


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--measurements',type=Path,required=True)
    parser.add_argument('--workspace',type=Path,required=True)
    parser.add_argument('--checker',type=Path,default=run.ROOT/'target/release/examples/check_config')
    parser.add_argument('--rusteda',type=Path,default=run.ROOT/'target/release/rusteda')
    parser.add_argument('--abc',type=Path,required=True)
    parser.add_argument('--cpu',type=int,default=0)
    parser.add_argument('--model-seconds',type=int,default=1800)
    parser.add_argument('--seconds',type=int,default=10800)
    parser.add_argument('--memory-gib',type=int,default=8)
    parser.add_argument('--swap-gib',type=int,default=1)
    parser.add_argument('--export-only',action='store_true',help='export checked obligations for exact sliced CEC; never counts as proof')
    parser.add_argument('--reimport-netlists',action='store_true',help='validate the exported Verilog for every tool through a fresh structural import')
    a=parser.parse_args()
    for name in ('measurements','workspace','checker','rusteda','abc'):setattr(a,name,getattr(a,name).resolve())
    if not 1<=a.model_seconds<=7200 or not 1<=a.seconds<=43200:parser.error('invalid wall budgets')
    if not 4<=a.memory_gib<=32 or not 0<=a.swap_gib<=4:parser.error('invalid physical memory/swap guard')
    main(a)
