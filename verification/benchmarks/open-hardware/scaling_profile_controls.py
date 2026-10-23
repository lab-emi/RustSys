#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Calibrate the reference flow's arbitrary-state behavior on both PDKs.

16 state bits, <100 cells, <1000 circuit entries *64B. Liberty parsing dominates
at <256MiB; one worker under 4GiB RAM/1GiB swap/12GiB AS and a 600s deadline.
The v1 X-next-state folding must produce an external counterexample, while v3
must retain all state and prove every output/event obligation. No reset exists.
"""
from pathlib import Path
import argparse
import os

import run
import transition
import yosys_initial


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--directory',type=Path,required=True)
    p.add_argument('--cpu',type=int,default=29)
    p.add_argument('--library',type=Path,action='append',help='explicit additional library; default covers both baseline PDKs')
    p.add_argument('--latch-map',type=Path,help='public techmap wrappers for an additional library')
    a=p.parse_args();out=a.directory.resolve();out.mkdir(parents=True,exist_ok=False)
    rtl=out/'initial_state_control.sv'
    rtl.write_text('''module initial_state_control(
 input clk,d,en,we,addr,input [3:0] din,
 output reg [6:0] q,output reg latch_q,output [3:0] rd);
 reg [3:0] mem [0:1];
 always @(posedge clk) begin
   q[0]<=1'bx; q[1]<=1'bz; q[2]<=0; q[3]<=1;
   q[4]<=d; q[5]<=d; if(en) q[6]<=d;
   if(we) mem[addr]<=din;
 end
 always @* if(en) latch_q=d;
 assign rd=mem[addr];
endmodule
''')
    root=run.ROOT;abc=root/'.work/lec-followup/oss-cad-20261014/oss-cad-suite/bin/yosys-abc'
    rows=[];tools=[root/'.work/open-hardware/yosys',root/'target/release/examples/check_config',abc]
    def execute(argv,d,label):
        unit=f'rustsys-profile-control-{os.getpid()}.scope'
        return run.execute(['systemd-run','--user','--scope','--quiet','--unit='+unit,
            '-p','MemoryMax=4G','-p','MemorySwapMax=1G','taskset','-c',str(a.cpu),*map(str,argv)],
            d,label,seconds=600,memory_gib=12,max_file_bytes=128*1024**2)
    libraries=a.library or [root/'target/pdk'/name for name in
        ('sky130_fd_sc_hd__tt_025C_1v80.lib','gf180mcu_fd_sc_mcu7t5v0__tt_025C_3v30.lib')]
    for library in libraries:
        library=library.resolve()
        for profile in ('v1','v3'):
            d=out/library.stem/profile;d.mkdir(parents=True)
            cfg={'directory':str(d),'design':{'top':'initial_state_control','files':[str(rtl)]},
                'library':str(library),'yosys':str(tools[0]),'yosys_bounded_map':False,
                'yosys_delay_ff_optimization':profile=='v3','normalize_case_promises':True}
            if a.latch_map:cfg['latch_map']=str(a.latch_map.resolve())
            run.save(d/'config.json',cfg)
            (d/'synth.ys').write_text(yosys_initial.script(cfg,d))
            synth=execute(['env','-C',d,tools[0],'-Q','-T','-s',d/'synth.ys'],d,'synth')
            if synth['status']!='ok':raise ValueError('reference control did not synthesize')
            cfg['export_obligations']=str(d/'obligations');run.save(d/'config.json',cfg)
            model=execute([tools[1],d/'config.json',d/'native.json'],d,'model')
            if model['status']!='ok' or not (d/'obligations/manifest.json').exists():
                raise ValueError('reference control did not export all state/event obligations')
            manifest=run.read_json(d/'obligations/manifest.json')
            command=transition.cec_command(transition.Limits(),120,d/'obligations/gold.aig',d/'obligations/gate.aig')
            checked=execute([abc,'-c',command],d,'abc')
            text=(d/'abc.stdout').read_text();yes=checked['status']=='ok' and transition.cec_equivalent(text)
            if profile=='v3':
                if not yes or manifest['gate_state_elements']!=16 or manifest['rtl_state_bits']!=16:
                    raise ValueError('v3 failed to preserve all 16 arbitrary state bits')
            elif yes or checked['status']!='ok' or 'Networks are NOT EQUIVALENT.' not in text:
                raise ValueError('v1 lacks the required external counterexample')
            if manifest['reset_assumptions'] or manifest['initial_state_choices']:
                raise ValueError('control introduced initial/reset assumptions')
            rows.append({'library':library.name,'profile':profile,'status':'passed',
                'expected':'equivalent' if profile=='v3' else 'external counterexample',
                'gate_states':manifest['gate_state_elements'],'rtl_states':manifest['rtl_state_bits'],
                'reset_assumptions':[],'initial_state_choices':[],
                'sha256':{str(x):run.digest(x) for x in (rtl,library,*tools,d/'netlist.v',d/'synth.ys',d/'obligations/manifest.json',d/'abc.stdout')},
                'measurement':checked})
            run.save(out/'profile-controls.json',{'status':'passed' if len(rows)==2*len(libraries) else 'incomplete','rows':rows})
            print(library.name,profile,'passed',flush=True)


if __name__=='__main__':main()
