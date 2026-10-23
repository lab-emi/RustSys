#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Complete native event LEC through independently proved arithmetic checkpoints.

One small core/corner at a time. Bounded graph storage<1GiB plus external
solvers; run this worker under aggregate4GiB RAM/1GiB swap/12GiB AS and21600s.
All original inputs/states/outputs/events remain. Observations add obligations.
Every extracted component and abstract context is reconstructed and externally
checked, all arithmetic is proved, and the actual measured netlist is checked
against the checkpoint. No partial stage, reset or initial-state choice passes.
"""
from pathlib import Path
from argparse import Namespace
from array import array
import argparse
import copy
import os
import time

import aig_lemmas
import large_lec
import mac_context
import observe_lec
import run
import transition

METHOD='abc-sca-independent-rtl-event-composition-v1'


def complete_export(p):
    c=p.get('certificate',{});m=c.get('manifest',{});k=m.get('obligation_kinds',{});n=m.get('gate_state_elements',0)
    return (p.get('method')==large_lec.METHOD and c.get('artifact_hashes_checked') is True
        and c.get('interface_header_checked') is True and c.get('gate_state_inventory_checked') is True
        and c.get('state_relation_coverage_checked') is True and m.get('state_pairs')==n and n>0
        and m.get('all_gate_initial_states_covered') is True and m.get('all_rtl_inputs_and_states_quantified') is True
        and m.get('reset_assumptions')==[] and m.get('initial_state_choices')==[] and m.get('case_assumptions')==[]
        and k.get('output',0)==m.get('output_bits') and k.get('output',0)>0
        and k.get('clock',0)+k.get('enable',0)==n and k.get('async-clear')==n and k.get('async-preset')==n
        and k.get('next-state',0)+k.get('data',0)==n and len(m.get('sides',[]))==2
        and all(s.get('inputs')==m.get('primary_input_bits',0)+m.get('rtl_state_bits',0)
            and s.get('outputs')==sum(k.values()) for s in m['sides']))


def complete(p):
    c=p.get('certificate',{});a=c.get('arithmetic',[]);checks=c.get('cec',[])
    return (p.get('method')==METHOD and p.get('status')=='ok' and p.get('verdict')=='proven'
        and p.get('scope',{}).get('reset_assumptions')=='none'
        and complete_export(c.get('checkpoint_export',{})) and complete_export(c.get('measured_export',{}))
        and c.get('same_original_frame_checked') is True and c.get('all_original_obligations_retained') is True
        and c.get('all_observed_operands_proved') is True and c.get('operand_independence_checked') is True
        and c.get('artifact_hashes_checked') is True and c.get('component_count',0)>0
        and len(a)==2*c['component_count'] and all(x.get('proven') is True for x in a)
        and len(checks)==2*c['component_count']+5 and all(x.get('proven') is True for x in checks)
        and p.get('partitions')==c['measured_export']['certificate']['manifest']['sides'][0]['outputs'])


def validate(p):
    if not complete(p):raise ValueError('incomplete observed arithmetic composition')
    for path,digest in {**p['source_sha256'],**p['artifact_sha256']}.items():
        if run.digest(path)!=digest:raise ValueError('composition input/artifact changed: '+path)
    for tool in p['tools'].values():
        if run.digest(tool['path'])!=tool['sha256']:raise ValueError('composition executable changed')
    for step in p['certificate']['cec']:
        if step['measurement']['status']!='ok' or not transition.cec_equivalent(Path(step['stdout']).read_text()):
            raise ValueError('missing complete external CEC verdict')
    for step in p['certificate']['arithmetic']:
        if step['measurement']['status']!='ok' or not mac_context.successful(Path(step['stdout']).read_text()):
            raise ValueError('missing complete external arithmetic verdict')
    return p


def prove(config,directory,checker,arisca,cpu=22,event_checker=None):
    d=Path(directory).resolve();d.mkdir(parents=True,exist_ok=False);started=time.monotonic()
    event_checker=Path(event_checker) if event_checker else run.ROOT/'target/release/examples/check_config'
    cfg=copy.deepcopy(config);netlist=Path(cfg['directory'])/'netlist.v'
    inputs=[*map(Path,cfg['design']['files']),Path(cfg['library']),netlist]
    pins={str(p):run.digest(p) for p in inputs}
    harness={str(run.HERE/name):run.digest(run.HERE/name) for name in ('observed_proof.py','aig_lemmas.py','observe_lec.py')}
    abc=run.ROOT/'.work/lec-followup/oss-cad-20261014/oss-cad-suite/bin/yosys-abc'
    tools={name:{'path':str(Path(path).resolve()),'sha256':run.digest(path)} for name,path in
        [('abc',abc),('arisca',arisca),('observation_checker',checker),('measurement_yosys',cfg['yosys']),
         ('checker',event_checker),('rusteda',run.ROOT/'target/release/rusteda')]}
    certificate={'arithmetic':[],'cec':[]}
    result={'method':METHOD,'status':'unverified','verdict':'unverified','partitions':0,
        'source_sha256':pins,'artifact_sha256':{},'tools':tools,'harness_sha256':harness,
        'certificate':certificate,'scope':{'reference':'independent two-valued RTL event model',
            'unknowns':'ordinary X/Z data and undriven wires completed to zero; language wildcards preserved',
            'initial_states':'all original RTL states quantified; every implementation state covered by checked correspondence',
            'reset_assumptions':'none','input_assumptions':'none','outputs':'all original output/state/event obligations',
            'arithmetic':'unconditional exact observed-operand lemmas, modular signed multiplication/addition',
            'wire_model':'none','corner':cfg['corner'],'drc_status':'unchecked'},
        'limits':{'physical_memory_gib':4,'swap_gib':1,'address_space_gib':12,'wall_s':21600,
                  'max_source_ands':aig_lemmas.MAX_ANDS,'max_component_nodes':aig_lemmas.MAX_CONE,
                  'polynomial_terms_per_worker':100000,'polynomial_workers':2}}
    def save(message=None):
        result['wall_s']=time.monotonic()-started
        if message:result['diagnostic']={'code':'BENCH_OBSERVED_LEC','message':message}
        run.save(d/'result.json',result);return result
    def remaining(cap):
        left=min(cap,21600-(time.monotonic()-started))
        if left<=0:raise ValueError('observed composition deadline')
        return left
    def cec(left,right,label,seconds=7200):
        before={str(x):run.digest(x) for x in (left,right)}
        command=transition.cec_command(transition.Limits(),remaining(seconds),left,right)
        measurement=run.execute(['taskset','-c',str(cpu),str(abc),'-c',command],d,label,
            seconds=remaining(seconds+5),memory_gib=12,max_file_bytes=64*1024**2)
        log=d/(label+'.stdout');yes=measurement['status']=='ok' and transition.cec_equivalent(log.read_text())
        if any(run.digest(path)!=digest for path,digest in before.items()):raise ValueError('CEC problem changed')
        certificate['cec'].append({'name':label,'proven':yes,'measurement':measurement,'stdout':str(log),'sha256':before})
        if not yes:raise ValueError('external CEC incomplete: '+label)
    try:
        checkpoint_cfg=copy.deepcopy(cfg)
        if cfg['tool']=='rustsys':
            checkpoint_cfg.update(yosys_flow='arbitrary-initial-state-full-map-v3',
                yosys_bounded_map=False,yosys_delay_ff_optimization=True,
                yosys_abc_fast=False,yosys_noshare=True,normalize_case_promises=True)
        pre=observe_lec.prepare(checkpoint_cfg,d/'checkpoint',checker,cpu)
        if not complete_export(pre):raise ValueError('checkpoint export is incomplete')
        certificate['checkpoint_export']=pre
        args=Namespace(checker=event_checker,rusteda=run.ROOT/'target/release/rusteda',
            abc=abc,cpu=cpu,model_seconds=1800,seconds=1800,memory_gib=4,swap_gib=1,
            export_only=True,reimport_netlists=True,require_full_core=False)
        post=large_lec.prove(cfg,d/'measured-export',args,{'netlist_sha256':run.digest(netlist)})
        if not complete_export(post):raise ValueError('actual measured netlist export is incomplete')
        certificate['measured_export']=post
        pre_dir=d/'checkpoint/export/obligations';post_dir=d/'measured-export/obligations'
        pm,qm=(p['certificate']['manifest'] for p in (pre,post))
        if ((pre_dir/'input-variables.u32le').read_bytes()!=(post_dir/'input-variables.u32le').read_bytes()
                or any(pm[k]!=qm[k] for k in ('output_bits','rtl_state_bits','gate_state_elements','state_pairs'))):
            raise ValueError('checkpoint/measured state or input frame differs')
        def state_frame(path):
            return [(x['rtl_index'],x['rtl_name'],x['rtl_variable']) for x in map(__import__('json').loads,path.read_text().splitlines())]
        if state_frame(pre_dir/'state-relation.jsonl')!=state_frame(post_dir/'state-relation.jsonl'):
            raise ValueError('checkpoint/measured state ordering differs')
        certificate['same_original_frame_checked']=True
        descriptions=aig_lemmas.macros(cfg['design']['id']);certificate['component_count']=len(descriptions)
        contexts={};mandatory=qm['sides'][0]['outputs']
        for side in ('gold','gate'):
            graph=aig_lemmas.Graph.read(pre_dir/(side+'.aig'))
            observed=aig_lemmas.observation_roots(graph,pm)
            values=[aig_lemmas.roles(observed,macro) for macro in descriptions]
            for macro,value in zip(descriptions,values):
                folder=d/'components'/side/macro['name'];folder.mkdir(parents=True)
                component,metadata=aig_lemmas.component(graph,value)
                component.write(folder/'component.aig');run.save(folder/'binding.json',metadata)
                # Check extraction AND argument binding against the exact source
                # with an independent Boolean engine, not only a self test.
                arguments=[x for role in ('a','b','c') for x in metadata['component_argument_literals'][role]]
                piece=aig_lemmas.Graph(component.inputs,component.ands,array('I',list(component.outputs)+arguments))
                restored=aig_lemmas.graft(graph,piece,[2*x for x in metadata['source_cut_nodes']])
                restored.write(folder/'restored.aig')
                expected=aig_lemmas.Graph(graph.inputs,graph.ands,array('I',value['y']+value['a']+value['b']+value['c']))
                expected.write(folder/'source.aig')
                cec(folder/'source.aig',folder/'restored.aig',side+'-'+macro['name']+'-extraction')
                workers=folder/'workers.toml';workers.write_text('workers = ["-m heuristic", "-m heuristic --flip"]\n')
                log=folder/'verdict.stdout'
                command=['env','RUST_LOG=info',str(arisca),str(folder/'component.aig'),'--spec',metadata['spec'],
                    '--portfolio','--portfolio-config',str(workers),'--size-limit','100000','--log-file',str(log)]
                measured=run.execute(['taskset','-c',str(cpu),*command],folder,'arisca',seconds=remaining(600),
                    memory_gib=12,max_file_bytes=64*1024**2)
                yes=measured['status']=='ok' and log.exists() and mac_context.successful(log.read_text())
                certificate['arithmetic'].append({'side':side,'macro':macro['name'],'proven':yes,'measurement':measured,
                    'stdout':str(log),'spec':metadata['spec'],'component_sha256':run.digest(folder/'component.aig')})
                if not yes:raise ValueError('arithmetic component incomplete: '+side+'/'+macro['name'])
            context,metadata=aig_lemmas.abstract_context(graph,values)
            context.write(d/(side+'-context.aig'));run.save(d/(side+'-context.json'),metadata)
            aig_lemmas.reconstruct_context(graph,context,metadata['source_result_literals']).write(d/(side+'-restored.aig'))
            cec(pre_dir/(side+'.aig'),d/(side+'-restored.aig'),side+'-context-reconstruction')
            # Drop only the supplementary observation ports for the exact
            # measured-netlist transport; all mandatory outputs are retained.
            aig_lemmas.Graph(graph.inputs,graph.ands,array('I',graph.outputs[:mandatory])).write(d/(side+'-mandatory.aig'))
            contexts[side]=metadata
        cec(d/'gold-context.aig',d/'gate-context.aig','full-Boolean-context')
        cec(d/'gold-mandatory.aig',post_dir/'gold.aig','original-source-frame')
        cec(d/'gate-mandatory.aig',post_dir/'gate.aig','actual-measured-netlist')
        certificate.update(all_original_obligations_retained=True,all_observed_operands_proved=True,
            operand_independence_checked=all(c['operand_independence_checked'] for c in contexts.values()))
        if any(run.digest(path)!=digest for path,digest in {**pins,**harness}.items()):raise ValueError('composition source changed')
        if any(run.digest(t['path'])!=t['sha256'] for t in tools.values()):raise ValueError('composition executable changed')
        for p in d.rglob('*'):
            if p.is_symlink():raise ValueError('composition artifact symlink')
            if p.is_file() and p.name!='result.json':result['artifact_sha256'][str(p)]=run.digest(p)
            if len(result['artifact_sha256'])>8192:raise ValueError('composition artifact-count budget')
        certificate['artifact_hashes_checked']=True
        result.update(status='ok',verdict='proven',partitions=mandatory)
        validate(result)
        return save()
    except (ValueError,OSError,MemoryError,KeyError) as error:
        result.update(status='unverified',verdict='unverified',partitions=0)
        return save(str(error))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    for name in ('config','directory','checker','arisca'):p.add_argument('--'+name,type=Path,required=True)
    p.add_argument('--event-checker',type=Path,help='independent complete event exporter; observation driver remains separate')
    p.add_argument('--cpu',type=int,default=22);a=p.parse_args()
    value=prove(run.read_json(a.config),a.directory,a.checker.resolve(),a.arisca.resolve(),a.cpu,a.event_checker)
    print(value['verdict'],value.get('diagnostic',''),flush=True)
    raise SystemExit(0 if complete(value) else 2)
