#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Prepare an observed pre-ABC checkpoint for independent event/SCA proofs.

Small-core plan: <=1M Boolean cells*~128B plus Liberty/parser/state buffers,
<2GiB RSS; one4GiB physical/1swap/12GiB-AS process at a time,1800s/filephase.
Only public Yosys commands and its standard exported Verilog are consumed.
Verification-only primitive functions supplement, never replace, PDK cells.
Preparing or exporting a checkpoint does not prove it correct.
"""
from pathlib import Path
from argparse import Namespace
import copy
import os
import subprocess

import equiv
import large_lec
import run
import yosys_initial

PRIMITIVES={
    'BUF':(['A'],'A'), 'NOT':(['A'],'!A'),
    'AND':(['A','B'],'A & B'), 'NAND':(['A','B'],'!(A & B)'),
    'OR':(['A','B'],'A | B'), 'NOR':(['A','B'],'!(A | B)'),
    'XOR':(['A','B'],'(A & !B) | (!A & B)'),
    'XNOR':(['A','B'],'!((A & !B) | (!A & B))'),
    'ANDNOT':(['A','B'],'A & !B'), 'ORNOT':(['A','B'],'A | !B'),
    'MUX':(['A','B','S'],'(!S & A) | (S & B)'),
    'NMUX':(['A','B','S'],'!((!S & A) | (S & B))'),
}


def primitive_library(original,destination):
    original,destination=map(Path,(original,destination))
    if original.stat().st_size>128*1024**2:raise ValueError('library byte budget')
    raw=original.read_text();end=raw.rfind('}')
    if end<0 or raw[end+1:].strip():raise ValueError('unsupported Liberty trailer')
    cells=[]
    for name,(inputs,function) in PRIMITIVES.items():
        full='$_'+name+'_'
        if full in raw:raise ValueError('verification primitive name collision')
        cells.append('cell ("'+full+'") { '+''.join('pin ('+pin+') { direction : input; } ' for pin in inputs)
            +'pin (Y) { direction : output; function : "'+function+'"; } }\n')
    destination.write_text(raw[:end]+'\n'+''.join(cells)+raw[end:])
    return {'original_library':str(original),'original_sha256':run.digest(original),
        'supplemented_library':str(destination),'supplemented_sha256':run.digest(destination),
        'original_bytes_preserved_except_insertion_before_final_brace':True,
        'primitive_functions':PRIMITIVES,'purpose':'Boolean verification only; no timing or area claims'}


def probes(design):
    if design=='vexriscv-verilog':
        widths={'execute_MulPlugin_aULow':16,'execute_MulPlugin_bULow':16,
            'execute_MulPlugin_aSLow':17,'execute_MulPlugin_bSLow':17,
            'execute_MulPlugin_aHigh':17,'execute_MulPlugin_bHigh':17,
            'execute_MUL_LL':32,'execute_MUL_LH':34,'execute_MUL_HL':34,'execute_MUL_HH':32}
    elif design=='ibex':
        base='u_ibex_core.ex_block_i.gen_multdiv_fast.multdiv_i.'
        widths={base+'gen_mult_fast.mult_op_a':16,base+'gen_mult_fast.mult_op_b':16,
            base+'sign_a':1,base+'sign_b':1,base+'accum':34,base+'mac_res_ext':34}
    else:raise ValueError('no arithmetic observation profile for this design')
    return [{'name':name,'offset':0,'width':width} for name,width in widths.items()]


def prepare(config,directory,checker,cpu=22):
    d=Path(directory).resolve();d.mkdir(parents=True,exist_ok=False);cfg=copy.deepcopy(config)
    cfg['directory']=str(d);original_library=Path(cfg['library'])
    script=yosys_initial.script(cfg,d)
    prefix,separator,_=script.partition('\nabc -liberty ')
    if (not separator or cfg.get('yosys_flow')!='arbitrary-initial-state-full-map-v3'
            or cfg.get('yosys_delay_ff_optimization') is not True
            or cfg.get('yosys_bounded_map') is not False or cfg.get('yosys_abc_fast') is not False
            or cfg.get('normalize_case_promises') is not True):
        raise ValueError('checkpoint requires the measured v3 default-mapping profile')
    script=prefix+'\nsetundef -zero -undriven\ncheck -assert\nwrite_verilog -noattr -noexpr '+equiv.quote(d/'netlist.v')+'\n'
    (d/'checkpoint.ys').write_text(script)
    command=['systemd-run','--user','--scope','--quiet',f'--unit=rustsys-observe-prefix-{os.getpid()}.scope',
        '-p','MemoryMax=4G','-p','MemorySwapMax=1G','taskset','-c',str(cpu),
        'env','-C',str(d),cfg['yosys'],'-Q','-T','-s',str(d/'checkpoint.ys')]
    try:step=run.execute(command,d,'checkpoint',seconds=1800,memory_gib=12,max_file_bytes=256*1024**2)
    finally:subprocess.run(['systemctl','--user','kill','--signal=KILL',f'rustsys-observe-prefix-{os.getpid()}.scope'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    if step['status']!='ok':raise ValueError('bounded pre-ABC checkpoint failed')
    derived=d/'verification-primitives.lib'
    run.save(d/'library-supplement.json',primitive_library(original_library,derived))
    cfg['library']=str(derived);cfg['observation_probes']=probes(cfg['design']['id'])
    run.save(d/'config.json',cfg)
    args=Namespace(checker=Path(checker),rusteda=run.ROOT/'target/release/rusteda',
        abc=run.ROOT/'.work/lec-followup/oss-cad-20261014/oss-cad-suite/bin/yosys-abc',
        cpu=cpu,model_seconds=1800,seconds=1800,memory_gib=4,swap_gib=1,
        export_only=True,reimport_netlists=True,require_full_core=False)
    result=large_lec.prove(cfg,d/'export',args,{'netlist_sha256':run.digest(d/'netlist.v')})
    m=result.get('certificate',{}).get('manifest',{})
    expected=cfg['observation_probes'];actual=m.get('observations',[])
    if len(actual)!=len(expected) or any({k:x[k] for k in ('name','offset','width')}!=p for x,p in zip(actual,expected)):
        raise ValueError('checkpoint observation export is incomplete: '+str(result.get('diagnostic')))
    if m['obligation_kinds'].get('observation')!=sum(p['width'] for p in expected):
        raise ValueError('checkpoint observation count differs')
    return result
