#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Dual-PDK controls for arithmetic/context proof composition.

<=1,000 cells/fixture * ~512 B * four copies <2 MiB; external workers <512 MiB.
Fixtures run serially under a 2 GiB physical limit and a 240 second deadline.
Wrong low/high bits, signedness, state, clocks and bindings must be rejected.
"""
from pathlib import Path
import argparse
import copy
import equiv
import mac_context
import run


def validate(args):
    out=args.workspace.resolve();out.mkdir(parents=True,exist_ok=False);rows=[]
    for label,lib in [('sky130','sky130_fd_sc_hd__tt_025C_1v80.lib'),
                      ('gf180','gf180mcu_fd_sc_mcu7t5v0__tt_025C_3v30.lib')]:
        for case in ('positive','wrong_low','wrong_high','wrong_sign','changed_state','changed_clock','wrong_binding'):
            d=out/label/case;d.mkdir(parents=True);top='mac_fixture'
            seed=d/'seed.v';snap=d/'snapshot.il';base_json=d/'base.json';base=d/'base.v'
            word_json=d/'word.json';word=d/'word.v'
            expr='($signed(a)*$signed(b)+$signed(c))'
            if case=='wrong_sign':expr='($signed({1\'b0,a})*$signed(b)+$signed(c))'
            if case=='wrong_low':expr+=' ^ 8\'h01'
            if case=='wrong_high':expr+=' ^ 8\'h80'
            seed.write_text('module mac_fixture(input[3:0]a,b,input[7:0]c,input clk,rst_n,en,'
                'output[7:0]y,output z);reg[2:0]q;assign y='+expr+';'
                'always@(posedge clk or negedge rst_n)if(!rst_n)q<=0;else q<=q^{2\'b0,a[0]};'
                'assign z=q[0]^en;endmodule\n')
            script=d/'prepare.ys';script.write_text(f'read_verilog {equiv.quote(seed)}\nhierarchy -top {top}\n'
                f'proc\nflatten\ntechmap\nopt_clean\nwrite_rtlil {equiv.quote(snap)}\n'
                f'setundef -zero -undriven\nscc -expect 0\ncheck -assert\nwrite_json {equiv.quote(base_json)}\n'
                f'write_verilog -noexpr -norename {equiv.quote(base)}\n')
            run.checked([str(args.yosys),'-Q','-T','-s',str(script)],d,'prepare',seconds=30,memory_gib=2)
            bindings={role:[[role,4 if role in ('a','b') else 8]] for role in ('a','b','c','y')}
            m=mac_context.load(base_json,top);vectors=mac_context.binding(m,bindings)
            w,_=mac_context.rewrite(m,vectors)
            if case in ('changed_state','changed_clock'):
                cell=next(c for c in w['cells'].values() if mac_context.STORAGE.fullmatch(c['type']))
                cell['connections']['D' if case=='changed_state' else 'C']=[vectors['a'][1]]
            if case=='wrong_binding':
                # The stored word expression still uses the correct bit order;
                # the claimed component binding must not silently reinterpret it.
                w['cells']['$checked_word_mul']['connections']['A']=list(reversed(vectors['a']))
            run.save(word_json,{'modules':{top:w}})
            script=d/'word.ys';script.write_text(f'read_json {equiv.quote(word_json)}\nhierarchy -top {top}\n'
                f'write_verilog -noexpr -norename {equiv.quote(word)}\n')
            run.checked([str(args.yosys),'-Q','-T','-s',str(script)],d,'word',seconds=30,memory_gib=2)
            p=d/'proof';r=mac_context.prove(p,snap,base_json,base,word_json,word,args.pdks/lib,top,
                    bindings,args.yosys,args.arisca,seconds=60,memory_gib=4)
            yes=r['verdict']=='proven';assert yes==(case=='positive'),(label,case,r)
            if yes:
                mac_context.validate(p,r)
                broken=copy.deepcopy(r);broken['partitions']-=1
                try:mac_context.validate(p,broken)
                except ValueError:pass
                else:raise AssertionError('omitted output/state accepted')
                broken=copy.deepcopy(r);broken['certificate']['bindings']['a']=[['b',4]]
                try:mac_context.validate(p,broken)
                except ValueError:pass
                else:raise AssertionError('rebound operand accepted')
                broken=copy.deepcopy(r);broken['source_sha256'].pop(str(word.resolve()))
                try:mac_context.validate(p,broken)
                except ValueError:pass
                else:raise AssertionError('missing source accepted')
            row={'library':label,'case':case,'expected':case=='positive','verdict':r['verdict'],
                 'wall_s':r['wall_s'],'diagnostic':r.get('diagnostic')};rows.append(row);print(row,flush=True)
    run.save(out/'validation.json',rows)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    for name in ('workspace','pdks','yosys','arisca'):p.add_argument('--'+name,type=Path,required=True)
    a=p.parse_args()
    for name in ('pdks','yosys','arisca'):setattr(a,name,getattr(a,name).resolve())
    validate(a)
