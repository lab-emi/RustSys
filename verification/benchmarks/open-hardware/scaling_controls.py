#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Independent dual-PDK positive/data/clock controls for mapped-state bridging.

The synthetic8-bit counter uses<100cells/1000nets, with at most1.1M Boolean
cells allowed by the checked transport. Planning<512MiB RSS per run including
Liberty parsing; guard2GiB RAM+1GiB swap,8GiB AS and1800s. One worker.
"""
from pathlib import Path
import argparse
import copy
import re
import sys

import report
import run
import transition


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--directory',type=Path,required=True)
    a=p.parse_args();out=a.directory.resolve();out.mkdir(parents=True,exist_ok=False)
    root=run.ROOT;suite=root/'.work/lec-followup/oss-cad-20261014/oss-cad-suite/bin'
    fixture=root/'verification/fixtures/synthetic/synth/rtl/counter/counter.sv'
    rows=[]
    for lib,name in [('sky130_fd_sc_hd__tt_025C_1v80.lib','sky130hd_tt'),('gf180mcu_fd_sc_mcu7t5v0__tt_025C_3v30.lib','gf180mcu7t_tt')]:
        d=out/name;d.mkdir();library=root/'target/pdk'/lib
        old=d/'gold.v'
        sample=run.execute([str(root/'target/release/rustsys'),'--threads','1','synth','--top','counter','--goal','area',
                            '--lib',str(library),'--out',str(old),str(fixture)],d,'synth-control',seconds=120,memory_gib=8)
        if sample['status']!='ok':raise ValueError('counter control did not synthesize')
        original=old.read_text()
        data,count=re.subn(r'(assign\s+wrap_o\s*=\s*)([^;]+);',lambda m:m[1]+'~('+m[2]+');',original)
        if count!=1:raise ValueError('expected exactly one wrap output to corrupt')
        start=original.index(');')+2
        clock=original[:start]+'\n  wire __bench_bad_clock;\n  assign __bench_bad_clock = ~clk_i;\n'+re.sub(r'\bclk_i\b','__bench_bad_clock',original[start:])
        for kind,text in [('correct',original),('bad-data',data),('bad-clock',clock)]:
            target=d/(kind+'.v');target.write_text(text)
            cfg={'design':{'top':'counter','files':[str(old)]},'library':str(library),
                 'netlist':str(target),'reference_gate_netlist':True,'tool':'rustsys'}
            result=transition.prove(cfg,d/kind,str(suite/'yosys'),str(suite/'eqy'),str(suite/'yosys-abc'),
                                    seconds=1800,memory_gib=8,limits=transition.Limits(whole_attempt_s=120))
            accepted=report.complete_external_proof(result)
            if kind=='correct' and not accepted:raise ValueError('positive control did not prove: '+str(result.get('diagnostic')))
            if kind!='correct' and (accepted or result.get('certificate',{}).get('equivalent') is not False):
                raise ValueError('negative control lacks an external inequivalence verdict: '+str(result.get('diagnostic')))
            rows.append({'library':name,'case':kind,'expected':'proven' if kind=='correct' else 'external inequivalence',
                         'accepted_as_equivalent':accepted,'status':'passed','result':result})
            run.save(out/'controls.json',{'status':'passed' if len(rows)==6 else 'incomplete','rows':rows})
            print(name,kind,'control passed',flush=True)


if __name__=='__main__':main()
