#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Independent positive/corruption controls for mixed word/ABC output proofs.

Eight fixtures across two PDKs, <256 cells each; one solver worker, planning
<512 MiB peak. Run under a 2 GiB physical-memory scope and a 300 s deadline.
"""
from pathlib import Path
import argparse
import copy
import sys
import equiv
import native_comb
import partition_comb
import report
import run


def validate(args):
    out=args.workspace.resolve();out.mkdir(parents=True,exist_ok=False);rows=[]
    for label,filename in [('sky130','sky130_fd_sc_hd__tt_025C_1v80.lib'),('gf180','gf180mcu_fd_sc_mcu7t5v0__tt_025C_3v30.lib')]:
        for case in ('positive','wrong_word','wrong_control','undefined_control'):
            p=out/label/case;p.mkdir(parents=True);gold=p/'rtl.v';seed=p/'seed.v';net=p/'netlist.v'
            prefix='module mixed(input [3:0]a,b,c,input en,bad,output [7:0]y,output z);wire[7:0]r=a*b+c;'
            control='(en?a[0]:b[0])'
            gold.write_text(prefix+'assign y=r;assign z='+control+';endmodule\n')
            y='r ^ 8\'h01' if case=='wrong_word' else 'r'
            z='~'+control if case=='wrong_control' else "bad?1'bx:"+control if case=='undefined_control' else control
            seed.write_text(prefix+'assign y='+y+';assign z='+z+';endmodule\n')
            script=p/'export.ys';script.write_text(f'read_verilog {equiv.quote(seed)}\nhierarchy -top mixed\nproc\nalumacc\nwrite_verilog -noexpr -norename {equiv.quote(net)}\n')
            run.checked([str(args.yosys),'-Q','-T','-s',str(script)],p,'export',seconds=30,memory_gib=2)
            cfg={'design':{'files':[str(gold)],'top':'mixed'},'library':str(args.pdks/filename),
                 'directory':str(p),'netlist':str(net),'tool':'yosys','gate_rtl':True,'gate_icells':True,'gate_refinement':True}
            d=p/'proof';r=partition_comb.prove(cfg,d,str(args.yosys),str(args.eqy),str(args.abc),('z',),seconds=120,memory_gib=4)
            yes=report.complete_external_proof(r);assert yes==(case=='positive'),(label,case,r)
            if yes:
                partition_comb.validate(d,r)
                assert not report.complete_external_proof(r['certificate']['native'])
                broken=copy.deepcopy(r);broken['certificate']['selected_outputs']=[]
                try:partition_comb.validate(d,broken)
                except ValueError:pass
                else:raise AssertionError('omitted control output accepted')
                model_path=d/'control-models/gold-comb.json';data=run.read_json(model_path);m=data['modules']['mixed'];cell=next(iter(m['cells'].values()));cell['attributes']['bad_fixture']=1;run.save(model_path,data)
                try:partition_comb.validate(d,r)
                except ValueError:pass
                else:raise AssertionError('changed projection graph accepted')
                cell['attributes'].pop('bad_fixture');run.save(model_path,data)
            rows.append({'library':label,'case':case,'expected':case=='positive','verdict':r['verdict'],'wall_s':r['wall_s']})
            print(rows[-1],flush=True)
    run.save(out/'validation.json',rows)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('workspace','pdks','yosys','eqy','abc'):parser.add_argument('--'+name,type=Path,required=True)
    args=parser.parse_args()
    for name in ('pdks','yosys','eqy','abc'):setattr(args,name,getattr(args,name).resolve())
    validate(args)
