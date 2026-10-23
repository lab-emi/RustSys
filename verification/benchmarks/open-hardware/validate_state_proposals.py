#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""External known-answer CEC for name-free, arbitrary-state export proposals.

Six one/two-FF fixtures, <100 circuit entries and <512 MiB planned RSS including
the library reader. One worker, 2 GiB AS / 120 s per child, <=1 MiB logs.
Random correspondence proposals are never a pass; ABC must prove/refute the
complete original output, data, clock and asynchronous-control obligations.
"""
from pathlib import Path
import argparse
import re

import run
import transition
import slice_lec


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--directory',type=Path,required=True)
    p.add_argument('--checker',type=Path,required=True)
    p.add_argument('--cec-solver',choices=('default','new'),default='default')
    p.add_argument('--abc',type=Path,default=run.ROOT/'.work/lec-followup/oss-cad-20261014/oss-cad-suite/bin/yosys-abc')
    a=p.parse_args();out=a.directory.resolve();out.mkdir(parents=True,exist_ok=False)
    lib=run.ROOT/'verification/fixtures/synthetic/synth/lib/wt_rich.lib'
    rtl=out/'source.sv'
    rtl.write_text('module m(input clk,d,output y); reg q; always @(posedge clk) q<=d; assign y=q; endmodule\n')
    rows=[]
    for kind in ('correct','bad-data','bad-clock','encoded-correct','encoded-bad-phase','encoded-bad-reset'):
        folder=out/kind;folder.mkdir();netlist=folder/'netlist.v'
        state='q' if kind=='bad-clock' else 'anonymous_state'
        data='nd' if kind=='bad-data' else 'd';clock='nc' if kind=='bad-clock' else 'clk'
        extra=('wt_inv_x1 inv(.A(d),.Y(nd));' if kind=='bad-data' else
               'wt_inv_x1 inv(.A(clk),.Y(nc));' if kind=='bad-clock' else '')
        netlist.write_text(f'module m(input clk,d,output y); wire {state},nd,nc; {extra} '
            f'wt_dff_x1 f(.CK({clock}),.D({data}),.Q({state})); assign y={state}; endmodule\n')
        cfg={'design':{'top':'m','files':[str(rtl)]},'netlist':str(netlist),
             'library':str(lib),'export_obligations':str(folder/'obligations')}
        if kind.startswith('encoded-'):
            source=folder/'source.sv'
            source.write_text('module m(input clk,rn,d,output y,z); reg q,r; always @(posedge clk or negedge rn) if(!rn) begin q<=1; r<=0; end else begin q<=d^r; r<=d^q; end assign y=q; assign z=r; endmodule\n')
            reset='d' if kind=='encoded-bad-reset' else 'rn'
            netlist.write_text('module m(input clk,rn,d,output y,z); wire a,b,da,db; wt_xnor2_x1 xa(.A(d),.B(b),.Y(da)); wt_xnor2_x1 xb(.A(d),.B(a),.Y(db)); wt_dffr_x1 fa(.CK(clk),.RN('+reset+'),.D(da),.Q(a)); wt_dffr_x1 fb(.CK(clk),.RN(rn),.D(db),.Q(b)); wt_inv_x1 iy(.A(a),.Y(y)); assign z=b; endmodule\n')
            proposals=folder/'proposals.json'
            run.save(proposals,[{'rtl_state':'q','gate_instance':'fa','inverted':kind!='encoded-bad-phase'},
                {'rtl_state':'r','gate_instance':'fb','inverted':False}])
            cfg.update(design={'top':'m','files':[str(source)]},state_proposals_file=str(proposals))
        run.save(folder/'config.json',cfg)
        model=run.execute([str(a.checker.resolve()),str(folder/'config.json'),str(folder/'native.json')],
            folder,'export',seconds=120,memory_gib=2,max_file_bytes=1024**2)
        native=run.read_json(folder/'native.json')
        if model['status']!='ok' or native['verdict']!='undecided':raise ValueError('export became a proof or failed')
        manifest=run.read_json(folder/'obligations/manifest.json')
        if (not manifest['all_gate_initial_states_covered'] or not manifest['all_rtl_inputs_and_states_quantified']
                or manifest['reset_assumptions'] or manifest['initial_state_choices'] or manifest['case_assumptions']
                or manifest['state_pairs']!=(2 if kind.startswith('encoded-') else 1)):raise ValueError('proposal narrowed the state/input scope')
        command=slice_lec.cec_command(120,folder/'obligations/gold.aig',folder/'obligations/gate.aig',a.cec_solver)
        checked=run.execute([str(a.abc.resolve()),'-c',command],folder,'abc',seconds=125,memory_gib=2,max_file_bytes=1024**2)
        text=(folder/'abc.stdout').read_text();extra_pins=[]
        if a.cec_solver=='new' and 'UNDECIDED' in text:
            command=slice_lec.cec_command(120,folder/'obligations/gold.aig',folder/'obligations/gate.aig')
            checked=run.execute([str(a.abc.resolve()),'-c',command],folder,'abc-fallback',seconds=125,memory_gib=2,max_file_bytes=1024**2)
            text=(folder/'abc-fallback.stdout').read_text();extra_pins=[folder/'abc-fallback.stdout',folder/'abc-fallback.command.json']
        yes=checked['status']=='ok' and transition.cec_equivalent(text)
        if kind in ('correct','encoded-correct'):
            if not yes:raise ValueError('correct anonymous-state design did not prove')
        elif yes or checked['status']!='ok' or not re.search(r'Networks are NOT (?:EQUIVALENT|equivalent)\.',text):
            raise ValueError('fault lacks explicit external inequivalence')
        pins=[Path(cfg['design']['files'][0]),netlist,lib,a.checker.resolve(),a.abc.resolve(),folder/'config.json',folder/'native.json',
              folder/'obligations/manifest.json',folder/'obligations/gold.aig',folder/'obligations/gate.aig',folder/'abc.stdout']
        if cfg.get('state_proposals_file'):pins.append(Path(cfg['state_proposals_file']))
        pins+=extra_pins
        rows.append({'case':kind,'status':'passed','accepted_as_equivalent':yes,'measurement':checked,
                     'sha256':{str(path):run.digest(path) for path in pins}})
        run.save(out/'controls.json',{'status':'passed' if len(rows)==6 else 'incomplete','rows':rows,
            'cec_solver':a.cec_solver,
            'scope':{'initial_states':'arbitrary','reset_assumptions':[],'input_assumptions':[]}})
        print(kind,'external control passed',flush=True)


if __name__=='__main__':main()
