#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Known-answer arithmetic and dual-PDK data/clock corruption controls.

One Ibex fixture at a time:~12k cells plus bounded JSON/library data, <512MiB
parent and<2GiB native/solver RSS. Run under4GiB aggregate RAM/1swap/12GiBAS,
one CPU/one corner and21600s. Mutants are separate files, never measurements.
Negative controls require explicit external inequivalence, never just a limit.
"""
from pathlib import Path
from argparse import Namespace
from array import array
import argparse
import copy
import re

import aig_lemmas
import equiv
import large_lec
import mac_context
import observed_proof
import run
import sta_structural
import transition


def main():
    p=argparse.ArgumentParser(description=__doc__)
    for name in ('directory','measurements','proofs','arisca'):p.add_argument('--'+name,type=Path,required=True)
    p.add_argument('--cpu',type=int,default=23);a=p.parse_args()
    out=a.directory.resolve();out.mkdir(parents=True,exist_ok=False);root=run.ROOT
    abc=root/'.work/lec-followup/oss-cad-20261014/oss-cad-suite/bin/yosys-abc'
    def execute(argv,d,label,seconds=600):
        return run.execute(['taskset','-c',str(a.cpu),*map(str,argv)],d,label,
            seconds=seconds,memory_gib=12,max_file_bytes=128*1024**2)
    first=run.read_json(a.measurements/'ibex/sky130hd_tt/16c/yosys-3/config.json')
    # An independent frontend supplies the signed multiply/add truth function.
    d=out/'arithmetic';d.mkdir();rtl=d/'known.sv'
    rtl.write_text("module known(input [16:0] a,b,input [33:0] c,output [33:0] y); wire signed [33:0] product; assign product=$signed(a)*$signed(b); assign y=product+c; endmodule\n")
    script=d/'prepare.ys';script.write_text('read_verilog -sv '+equiv.quote(rtl)+'\nhierarchy -top known\nproc\nflatten\nopt_expr -keepdc\nopt_clean\nalumacc\ntechmap\nopt_expr -keepdc\nopt_clean\naigmap\nopt_clean\nwrite_aiger -symbols '+equiv.quote(d/'reference.aig')+'\n')
    value=execute([first['yosys'],'-Q','-T','-s',script],d,'prepare')
    if value['status']!='ok':raise ValueError('arithmetic control frontend failed')
    interface=transition.aig_interface(d/'reference.aig',transition.Limits())
    if interface['inputs']!=[f'{name}[{bit}]' for name,width in (('a',17),('b',17),('c',34)) for bit in range(width)]:
        raise ValueError('arithmetic fixture input order differs')
    graph=aig_lemmas.Graph.read(d/'reference.aig');arithmetic=[]
    spec=f'({aig_lemmas.word_spec([2*(k+1) for k in range(17)],True)})*({aig_lemmas.word_spec([2*(k+1) for k in range(17,34)],True)})+({aig_lemmas.word_spec([2*(k+1) for k in range(34,68)],False)})'
    for kind,flip in [('correct',None),('bad-low-bit',0),('bad-high-bit',33)]:
        folder=d/kind;folder.mkdir();outputs=array('I',graph.outputs)
        if flip is not None:outputs[flip]^=1
        aig_lemmas.Graph(graph.inputs,graph.ands,outputs).write(folder/'component.aig')
        workers=folder/'workers.toml';workers.write_text('workers = ["-m heuristic", "-m heuristic --flip"]\n')
        log=folder/'verdict.stdout'
        value=execute(['env','RUST_LOG=info',a.arisca.resolve(),folder/'component.aig','--spec',spec,
            '--portfolio','--portfolio-config',workers,'--size-limit','100000','--log-file',log],folder,'arisca')
        text=log.read_text() if log.exists() else '';yes=value['status']=='ok' and mac_context.successful(text)
        if (kind=='correct' and not yes) or (kind!='correct' and (yes or value['status']!='ok' or 'Verification failed!' not in text)):
            raise ValueError('arithmetic signed/overflow control failed: '+kind)
        arithmetic.append({'case':kind,'status':'passed','accepted_as_equivalent':yes,'spec':spec,
            'sha256':{str(x):run.digest(x) for x in (rtl,script,d/'reference.aig',folder/'component.aig',log,a.arisca.resolve())}})
        print('Arithmetic',kind,'passed',flush=True)
    run.save(out/'arithmetic-controls.json',{'status':'passed','rows':arithmetic,
        'scope':'all Boolean input valuations of the production signed17x17 +34bit accumulator modulo2^34 signature; no assumptions'})
    rows=[]
    for lib in ('sky130hd_tt','gf180mcu7t_tt'):
        case=out/lib;case.mkdir();cfg=run.read_json(a.measurements/'ibex'/lib/'16c/yosys-3/config.json')
        baseline_dir=a.proofs/'ibex'/lib/'yosys';baseline=observed_proof.validate(run.read_json(baseline_dir/'result.json'))
        rows.append({'library':lib,'case':'correct','status':'passed','accepted_as_equivalent':True,
            'baseline_sha256':run.digest(baseline_dir/'result.json')})
        # Obtain a real standard-cell inverter from the public binary rather
        # than assuming a PDK's cell or pin spelling.
        inv=case/'inverter.sv';inv.write_text('module inv_control(input clk_i,output y); assign y=~clk_i; endmodule\n')
        script=case/'inverter.ys';script.write_text('read_liberty -lib '+equiv.quote(cfg['library'])+'\nread_verilog '+equiv.quote(inv)+'\nhierarchy -top inv_control\nproc\ntechmap\nabc -liberty '+equiv.quote(cfg['library'])+'\nclean\nwrite_json '+equiv.quote(case/'inverter.json')+'\n')
        value=execute([cfg['yosys'],'-Q','-T','-s',script],case,'inverter')
        if value['status']!='ok':raise ValueError('PDK inverter control failed')
        model=run.read_json(case/'inverter.json')['modules']['inv_control']
        if len(model['cells'])!=1:raise ValueError('inverter must use one mapped cell')
        cell=next(iter(model['cells'].values()));clk=model['ports']['clk_i']['bits'];y=model['ports']['y']['bits']
        if cell['type'].startswith('$'):raise ValueError('inverter control is unmapped')
        pins=[]
        for name,bits in cell['connections'].items():
            if not re.fullmatch(r'[A-Za-z_][A-Za-z0-9_$]*',name) or len(bits)!=1:raise ValueError('inverter pin shape')
            net='clk_i' if bits==clk else '__lec_clock_fault' if bits==y else None
            if net is None:raise ValueError('unexpected inverter internal net')
            pins.append('.'+name+'('+net+')')
        original=Path(cfg['directory'])/'netlist.v';text=original.read_bytes()
        if b'__lec_clock_fault' in text:raise ValueError('control namespace collision')
        changed,count=re.subn(rb'(assign\s+alert_minor_o\s*=\s*)1\x27h0\s*;',lambda m:m[1]+b"1'h1;",text)
        if count!=1:raise ValueError('expected one constant alert_minor_o output')
        # Keep declaration and escaped hierarchical identifiers intact.
        header=text.index(b');')+2;body=[];replacements=0
        for line in text[header:].splitlines(keepends=True):
            if re.match(rb'\s*(?:input|output|inout|wire)\b',line):body.append(line);continue
            def token(match):
                nonlocal replacements
                if match[0]==b'clk_i':replacements+=1;return b'__lec_clock_fault'
                return match[0]
            body.append(sta_structural.TOKEN.sub(token,line))
        if not replacements:raise ValueError('no clock uses changed')
        injection=('\n wire __lec_clock_fault;\n '+cell['type']+' __lec_clock_fault_inv ('+', '.join(pins)+');\n').encode()
        clock=text[:header]+injection+b''.join(body)
        for kind,data in [('bad-data',changed),('bad-clock',clock)]:
            folder=case/kind;folder.mkdir();(folder/'netlist.v').write_bytes(data)
            mutated=copy.deepcopy(cfg);mutated['directory']=str(folder)
            args=Namespace(checker=root/'target/release/examples/check_config',rusteda=root/'target/release/rusteda',
                abc=abc,cpu=a.cpu,model_seconds=1800,seconds=1800,memory_gib=4,swap_gib=1,
                export_only=True,reimport_netlists=True,require_full_core=False)
            export=large_lec.prove(mutated,folder/'export',args,{'netlist_sha256':run.digest(folder/'netlist.v')})
            if not observed_proof.complete_export(export):raise ValueError('mutant event export incomplete')
            command=transition.cec_command(transition.Limits(),120,baseline_dir/'gate-mandatory.aig',folder/'export/obligations/gate.aig')
            value=execute([abc,'-c',command],folder,'abc',seconds=125);log=(folder/'abc.stdout').read_text()
            if value['status']!='ok' or 'Networks are NOT EQUIVALENT.' not in log or transition.cec_equivalent(log):
                raise ValueError('mutant lacks an external inequivalence verdict: '+kind)
            rows.append({'library':lib,'case':kind,'status':'passed','accepted_as_equivalent':False,
                'expected':'external inequivalence','original_netlist_sha256':run.digest(original),
                'mutant_sha256':run.digest(folder/'netlist.v'),'measurement':value,
                'sha256':{str(x):run.digest(x) for x in (folder/'netlist.v',folder/'abc.stdout',baseline_dir/'gate-mandatory.aig',Path(cfg['library']))}})
            print(lib,kind,'passed',flush=True)
        run.save(out/'controls.json',{'status':'passed' if len(rows)==6 else 'incomplete','rows':rows,
            'scope':'complete native arbitrary-state event frame; all mandatory ports/state/data/clocks/resets; no assumptions'})


if __name__=='__main__':main()
