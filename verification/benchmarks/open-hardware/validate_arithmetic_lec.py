#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Small end-to-end arithmetic composition controls on both baseline PDKs.

Each fixture has 10 output bits and below 500 nodes. Expected RSS <256 MiB;
use an outer 2 GiB cgroup and a 600 second wall guard, one worker at a time.
"""
import argparse
from pathlib import Path
import re
import sys

import arithmetic_lec
import run


def main(args):
    args.workspace.mkdir(parents=True,exist_ok=False)
    source = args.workspace/'multiply.v'
    source.write_text("module t(input clk,rst_n,input signed [4:0] a,input [3:0] b,output signed [9:0] y);reg signed [4:0] operand;always @(posedge clk or negedge rst_n)if(!rst_n)operand<=0;else operand<=a;assign y=operand*$signed({1'b0,b});endmodule\n")
    results=[]
    for pdk,filename in [('sky130','sky130_fd_sc_hd__tt_025C_1v80.lib'),
                         ('gf180','gf180mcu_fd_sc_mcu7t5v0__tt_025C_3v30.lib')]:
        d=args.workspace/pdk;d.mkdir()
        config={'tool':'rustsys','directory':str(d),'library':str(args.pdks/filename),'corner':'control',
                'rustsys':str(args.rustsys),'rusteda':str(args.rusteda),
                'design':{'top':'t','files':[str(source)]}}
        run.save(d/'config.json',config)
        built=run.execute([sys.executable,str(run.HERE/'run.py'),'_pipeline',str(d/'config.json')],d,'synth',seconds=60,memory_gib=2)
        if built['status']!='ok':raise RuntimeError('arithmetic control synthesis failed')
        netlist=d/'netlist.v';text=netlist.read_text();text=re.sub(r'\by\b','original_y',text)
        head,body=text.split(');',1)
        if head.count('original_y')!=1:raise RuntimeError('arithmetic output mutation failed')
        text=head.replace('original_y','y')+');\nwire [9:0] original_y;\nassign y=~original_y;\n'+body
        mutant=d/'mutant.v';mutant.write_text(text)
        for name,path,expected in [('positive',netlist,'proven'),('negative',mutant,'unverified')]:
            c={**config,'netlist':str(path)}
            proof=arithmetic_lec.prove(c,d/name,args.exporter,args.yosys,args.eqy,args.abc,
                                       args.amulet,args.pacheck,seconds=120,memory_gib=2,max_cells=20000)
            if proof['verdict']!=expected:raise RuntimeError(f'unexpected arithmetic result: {pdk} {name}')
            results.append({'pdk':pdk,'case':name,'verdict':proof['verdict']})
            print(pdk,name,proof['verdict'],flush=True)
    run.save(args.workspace/'validation.json',results)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    for name in ['workspace','pdks','rustsys','rusteda','exporter','yosys','eqy','abc','amulet','pacheck']:
        p.add_argument('--'+name,type=Path,required=True)
    args=p.parse_args()
    for name,value in vars(args).items():setattr(args,name,value.resolve())
    main(args)
