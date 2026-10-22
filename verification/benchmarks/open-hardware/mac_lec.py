#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Reproduce original RTL -> checked MAC word model -> measured netlist.

Memory/resource planning is documented in staged_lec.py/native_comb.py and
mac_context.py. One whole-design solver at a time; <=100k cells, <=2M graph
connections, <=256 retired optional names, <=64 independent control outputs.
The CLI must run under a 4 GiB physical-memory scope and a wall-clock guard.
"""
from pathlib import Path
import argparse
import copy
import time
import arithmetic_lec
import equiv
import mac_context
import native_comb
import partition_comb
import report
import run
import staged_lec
import transition


def prove(config,workspace,bindings,synthesis_yosys,yosys,eqy,abc,arisca,*,prefix=None,seconds=10800,memory_gib=8,unpair=()):
    d=Path(workspace).resolve();d.mkdir(parents=True,exist_ok=False);start=time.monotonic()
    original=Path(config['design']['files'][0]).resolve();library=Path(config['library']).resolve()
    target=Path(config.get('netlist',Path(config['directory'])/'netlist.v')).resolve()
    top=equiv.object_name(config['design']['top'])
    def remaining(cap):
        left=min(cap,seconds-(time.monotonic()-start))
        if left<=0:raise ValueError('MAC composition deadline reached')
        return left
    def execute(script,label):
        p=d/(label+'.ys');p.write_text(script)
        run.checked([str(yosys),'-Q','-T','-s',str(p)],d,label,seconds=remaining(120),memory_gib=memory_gib)
    if prefix is None:
        prefix=d/'prefix'
        r=staged_lec.prove(config,prefix,synthesis_yosys,yosys,eqy,abc,seconds=remaining(seconds),
                           memory_gib=memory_gib,through_stage=7,unpair=unpair)
        if not report.complete_external_proof(r):raise ValueError('original RTL prefix remains unverified')
    prefix=Path(prefix).resolve();prefix_proof=prefix/'chain-07'
    arithmetic_lec.external(prefix_proof)
    paths={n:d/f for n,f in [('base_json','base.json'),('base','base.v'),('word_json','word.json'),('word','word.v')]}
    paths['snapshot']=prefix/'s16.il'
    execute(f'read_rtlil {equiv.quote(paths["snapshot"])}\nhierarchy -top {top}\nsetundef -zero -undriven\n'
            f'write_json {equiv.quote(paths["base_json"])}\nwrite_verilog -noexpr -norename {equiv.quote(paths["base"])}\n','base')
    model=mac_context.load(paths['base_json'],top);vectors=mac_context.binding(model,bindings)
    word,_=mac_context.rewrite(model,vectors);run.save(paths['word_json'],{'modules':{top:word}})
    del model,word
    execute(f'read_json {equiv.quote(paths["word_json"])}\nhierarchy -top {top}\nscc -expect 0\ncheck -assert\n'
            f'write_verilog -noexpr -norename {equiv.quote(paths["word"])}\n','word')
    component=d/'arithmetic'
    r=mac_context.prove(component,paths['snapshot'],paths['base_json'],paths['base'],paths['word_json'],paths['word'],
                        library,top,bindings,yosys,arisca,seconds=remaining(180),memory_gib=memory_gib)
    if not report.complete_external_proof(r):raise ValueError('MAC component/context remains unverified')
    cfg=copy.deepcopy(config);cfg['design']['files']=[str(prefix/'s07.v')];cfg['netlist']=str(paths['word'])
    cfg.update(rtl_icells=True,gate_icells=True,gate_rtl=True,gate_refinement=True)
    source=d/'source';r=native_comb.prove(cfg,source,str(yosys),str(eqy),str(abc),seconds=remaining(1800),memory_gib=memory_gib)
    if not report.complete_external_proof(r):
        retired=native_comb.failed_hints(source);selected=native_comb.failed_outputs(source)
        source=d/'source-refined'
        if selected:
            r=partition_comb.prove(cfg,source,str(yosys),str(eqy),str(abc),selected,
                internal_exclusions=retired,seconds=remaining(1800),memory_gib=memory_gib)
        else:
            r=native_comb.prove(cfg,source,str(yosys),str(eqy),str(abc),internal_exclusions=retired,
                seconds=remaining(1800),memory_gib=memory_gib)
    if not report.complete_external_proof(r):raise ValueError('original RTL to word model remains unverified')
    arithmetic_lec.compose_transitive(d/'chain-word',prefix_proof,source,[original],prefix/'s07.v',paths['word'],library)
    arithmetic_lec.compose_transitive(d/'chain-base',d/'chain-word',component,[original],paths['word'],paths['base'],library)
    cfg=copy.deepcopy(config);cfg['design']['files']=[str(paths['base'])];cfg['netlist']=str(target);cfg['rtl_icells']=True
    mapped=d/'mapped';mapped.mkdir();run.save(mapped/'input.json',cfg)
    r=transition.prove(cfg,mapped,str(yosys),str(eqy),str(abc),seconds=remaining(1800),memory_gib=memory_gib,
                       limits=transition.Limits(max_batch_outputs=64,whole_attempt_s=600))
    if not report.complete_external_proof(r):raise ValueError('Boolean model to measured netlist remains unverified')
    r=arithmetic_lec.compose_transitive(d/'complete',d/'chain-base',mapped,[original],paths['base'],target,library)
    arithmetic_lec.external(d/'complete');run.save(d/'result.json',r);return r


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    for n in ('config','workspace','bindings','synthesis-yosys','yosys','eqy','abc','arisca'):
        p.add_argument('--'+n,type=Path,required=True)
    p.add_argument('--prefix',type=Path,help='reuse a separately proved checkpoint prefix')
    p.add_argument('--timeout',type=int,default=10800);p.add_argument('--unpair-constant',action='append',default=[])
    a=p.parse_args()
    try:
        r=prove(run.read_json(a.config),a.workspace,run.read_json(a.bindings),
                *[getattr(a,n).resolve() for n in ('synthesis_yosys','yosys','eqy','abc','arisca')],
                prefix=a.prefix,seconds=a.timeout,unpair=a.unpair_constant)
    except (ValueError,KeyError,OSError,MemoryError) as error:
        r={'method':arithmetic_lec.TRANSITIVE_METHOD,'status':'unverified','verdict':'unverified',
           'diagnostic':{'code':'BENCH_MAC_LEC','message':str(error)},'scope':{'reset_assumptions':'none'}}
        a.workspace.mkdir(parents=True,exist_ok=True)
        if not (a.workspace/'result.json').exists():run.save(a.workspace/'result.json',r)
    print(r['verdict']);raise SystemExit(0 if r['verdict']=='proven' else 2)
