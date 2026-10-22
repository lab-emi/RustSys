#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Independent Yosys-model/ABC event LEC for enabled/feedback FF fixtures.

Two PDKs,18states/fixture,<=20Kcells/256Kbits: expected<1GiB per sequential
worker; launch with an aggregate4GiBphysical/1GiBswap/12GiB-AS guard. Each
proof has600s and explicit graph/JSON/SAT limits. Intentional enable/clock
faults require an external counterexample, not a timeout or preparation error.
"""
from pathlib import Path
import argparse
import re
import tomllib

import run
import report
import transition


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--fixtures',type=Path,required=True)
    p.add_argument('--directory',type=Path,required=True)
    p.add_argument('--suite',type=Path,required=True)
    a=p.parse_args();out=a.directory.resolve();out.mkdir(parents=True,exist_ok=False)
    pdks={r['name']:r for r in tomllib.loads((run.ROOT/'verification/pdks.toml').read_text())['library']}
    rows=[];limits=transition.Limits(max_cells=20000,max_bits=262144,max_state=64,
        max_connections=200000,max_json_bytes=64*1024**2,whole_attempt_s=120,cec_engine='legacy')
    for name in ('sky130hd_tt','gf180mcu7t_tt'):
        fixture=(a.fixtures/name).resolve();metadata=run.read_json(fixture/'metadata.json')
        lib=Path(metadata['library']);rtl=fixture/'rtl.v';netlist=fixture/'netlist.v'
        if run.digest(lib)!=pdks[name]['sha256']:raise ValueError('PDK is not the pinned baseline')
        if max(rtl.stat().st_size,netlist.stat().st_size)>1024**2:raise ValueError('fixture byte budget')
        cfg={'design':{'id':'enabled-seq-regression','name':'enabled sequential regression',
             'top':metadata['top'],'files':[str(rtl)]},'library':str(lib),'corner':pdks[name]['corner'],
             'netlist':str(netlist),'tool':'rustsys'}
        for mode,signal in (('correct',None),('bad-enable','en'),('bad-clock','clk')):
            case=out/name/mode;case.mkdir(parents=True)
            current=dict(cfg)
            if signal:
                text=netlist.read_text()
                changed,n=re.subn(r'\('+re.escape(signal)+r'\)',"(1'b0)",text,count=1)
                if n!=1:raise ValueError('expected connected primary signal for deliberate mutation')
                mutant=case/'mutant.v';mutant.write_text(changed);current['netlist']=str(mutant)
            run.save(case/'config.json',current)
            result=transition.prove(current,case/'proof',a.suite/'bin/yosys',a.suite/'bin/eqy',a.suite/'bin/yosys-abc',
                                    seconds=600,memory_gib=8,limits=limits)
            proven=report.complete_external_proof(result)
            counterexample=any('Networks are NOT EQUIVALENT' in log.read_text() or 'Verification failed' in log.read_text()
                for log in (case/'proof').rglob('*.stdout') if log.stat().st_size<1024**2)
            passed=proven if mode=='correct' else counterexample and not proven
            rows.append({'library':name,'mode':mode,'passed':passed,'proof_complete':proven,
                         'external_counterexample':counterexample,'result':result})
            run.save(out/'results.json',{'status':'passed' if len(rows)==6 and all(r['passed'] for r in rows) else 'incomplete',
                'scope':'all outputs and state/event obligations, arbitrary initial states, no reset/input assumptions','rows':rows})
            print(name,mode,'passed',passed,'proven',proven,'counterexample',counterexample,flush=True)
            if not passed:raise SystemExit('enabled-state external validation incomplete')


if __name__=='__main__':main()
