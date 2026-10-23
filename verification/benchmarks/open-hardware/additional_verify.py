#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Fresh arbitrary-state LEC and STA for a complete additional-library campaign.

<=5 rows / 10 netlists, <128 MiB parent. Existing bounded workers handle each
case: small LEC 4 GiB, full-core export 24 GiB, STA 32 GiB maximum, serial.
Full-core models/STA share scale.lock. No baseline certificate is reused for
a changed library; exported Verilog, original RTL and Liberty are all bound.
"""
from pathlib import Path
import argparse
import fcntl
import re
import copy

import run
import scaling_report
import scaling_verify
import functional_models


def measurement_rows(data, library):
    if data['limits']['repetitions']!=3 or data['limits']['warmups']!=1:
        raise ValueError('additional campaign needs one warm-up and three measurements')
    rows={r['id']:r for r in data['rows'] if r['library']==library}
    if len(data['rows'])!=5 or set(rows)!=set(scaling_report.ORDER):
        raise ValueError('additional campaign must cover all five designs without duplicate rows')
    for row in rows.values():
        if row['allocated_physical_cores']!=16 or len(row['cpu_affinity'])!=16:
            raise ValueError('additional comparison requires sixteen physical cores')
        for tool in ('rustsys','yosys'):
            result=row.get(tool,{})
            if (result.get('status')!='ok' or len(result.get('samples',[]))!=3
                    or any(s['status']!='ok' or s['netlist_sha256']!=result['netlist_sha256']
                           for s in result['samples'])):
                raise ValueError('incomplete or nonidentical measured repetitions')
            scaling_report.peak_memory_mib(result)
    return rows


def checked_config(workspace, row, tool):
    folder=workspace/row['id']/row['library']/'16c'/f'{tool}-3'
    cfg=run.read_json(folder/'config.json')
    if cfg['threads']!=16 or Path(cfg['directory']).resolve()!=folder:
        raise ValueError('measured configuration differs')
    digest=row[tool]['netlist_sha256']
    for repetition in (0,1,2,3):
        sample_dir=folder.parent/f'{tool}-{repetition}'
        sample=run.read_json(sample_dir/'sample.json')
        current=run.read_json(sample_dir/'config.json')
        if (current['threads']!=16 or current['tool']!=tool or current['design']!=cfg['design']
                or current['library']!=cfg['library'] or current['corner']!=cfg['corner']
                or Path(current['directory']).resolve()!=sample_dir):
            raise ValueError('a repeated sample used a different configuration')
        if sample['status']!='ok' or sample['netlist_sha256']!=digest or run.digest(sample_dir/'netlist.v')!=digest:
            raise ValueError('measured or warm-up artifact differs')
    return cfg


def check_hashes(mapping):
    for path,digest in mapping.items():
        if run.digest(path)!=digest:raise ValueError('input/tool/evidence changed: '+path)


def main():
    p=argparse.ArgumentParser(description=__doc__)
    for name in ('workspace','directory','sta'):p.add_argument('--'+name,type=Path,required=True)
    p.add_argument('--library',default='ez130_8t_tt')
    p.add_argument('--phase',choices=('proofs','timing','all'),default='all')
    p.add_argument('--event-checker',type=Path,help='explicit frozen event checker for freshly discovered state names')
    p.add_argument('--state-proposals-file',type=Path,help='bounded untrusted OpenC910/Yosys matching hints; full external CEC remains required')
    p.add_argument('--cec-solver',choices=('default','new'),default='default',help='full-core CEC algorithm; new uses public -x first with bounded default fallback')
    p.add_argument('--attempt',help='new bounded attempt directory; preserve earlier failed receipts in place')
    p.add_argument('--state-discovery-work',type=int,default=64000000000,help='full-core export proposal work cap; no proof assumptions are changed')
    a=p.parse_args();ws=a.workspace.resolve();out=a.directory.resolve();out.mkdir(parents=True,exist_ok=True)
    if a.attempt and not re.fullmatch(r'[a-zA-Z0-9_-]+',a.attempt):p.error('invalid shell-safe attempt handle')
    if not 1<=a.state_discovery_work<=256000000000:p.error('invalid proposal work budget')
    if a.state_proposals_file and a.state_proposals_file.stat().st_size>8*1024**2:
        p.error('state proposal file byte budget')
    data=run.read_json(ws/'results.json');rows=measurement_rows(data,a.library)
    check_hashes({**data['input_sha256'],**data['binary_sha256']})
    for record in data['reference_executables']:check_hashes({record['path']:record['sha256']})
    proofs={'schema':1,'scope':{'initial_states':'arbitrary','reset_assumptions':'none','input_assumptions':'none'},'rows':[]}
    timing={'schema':1,'rows':[]}
    original_cfg=checked_config(ws,rows[scaling_report.ORDER[0]],'yosys')
    model_directory=out/('functional-models-'+a.attempt if a.attempt else 'functional-models')
    supplement=functional_models.prepare(original_cfg['library'],model_directory,original_cfg['yosys'])
    for did in scaling_report.ORDER:
        row=rows[did];pr={'id':did,'library':a.library};tr=dict(pr)
        for tool in ('yosys','rustsys'):
            measured_cfg=checked_config(ws,row,tool);cfg=copy.deepcopy(measured_cfg);dest=out/did/a.library/tool
            if a.attempt:dest=dest/a.attempt
            if supplement:cfg['library']=supplement['functional_library']
            if did=='openc910':
                cfg['export_discovery_work']=a.state_discovery_work;cfg['cec_solver']=a.cec_solver
            if did=='openc910' and tool=='yosys' and a.state_proposals_file:
                cfg['state_proposals_file']=str(a.state_proposals_file.resolve())
            if a.phase in ('proofs','all'):
                print('Fresh arbitrary-state LEC:',did,tool,flush=True)
                if tool=='yosys' and did in ('ibex','vexriscv-verilog'):
                    proof=scaling_verify.observed_case(cfg,dest/'composed',event_checker=a.event_checker)
                else:
                    native=dest/'native'
                    if (native/'result.json').exists():proof=run.read_json(native/'result.json')
                    else:
                        with (run.ROOT/'.work/scale.lock').open('a') as lock:
                            if did=='openc910':fcntl.flock(lock,fcntl.LOCK_EX)
                            proof=scaling_verify.native_case(cfg,native,row[tool]['samples'][-1],a.event_checker)
                    if not scaling_verify.complete(proof) and did in ('ibex','vexriscv-verilog'):
                        logs=(native/'abc-cec.stdout',native/'export/abc-cec.stdout')
                        if any(any(marker in path.read_text() for marker in ('NOT EQUIVALENT','Verification failed'))
                               for path in logs if path.exists()):
                            raise ValueError('native counterexample requires investigation')
                        proof=scaling_verify.observed_case(cfg,dest/'composed',event_checker=a.event_checker)
                if not scaling_verify.complete(proof):raise ValueError('incomplete arbitrary-state LEC: '+str(dest))
                proof=functional_models.bind(proof,supplement)
                check_hashes(proof['source_sha256'])
                if row[tool]['netlist_sha256'] not in proof['source_sha256'].values():
                    raise ValueError('LEC omits measured netlist')
                pr[tool]=proof
                print('Proven:',did,tool,proof['method'],flush=True)
            if a.phase in ('timing','all'):
                sta_dir=dest/'sta'
                if (sta_dir/'result.json').exists():result=run.read_json(sta_dir/'result.json')
                else:
                    import sta_benchmark
                    result=sta_benchmark.measure(Path(measured_cfg['directory'])/'netlist.v',measured_cfg['library'],cfg['design']['top'],
                        scaling_verify.CLOCK[did],a.sta,sta_dir,memory_gib=32 if did=='openc910' else 4,
                        seconds=7200,cpu=31)
                if result['status']!='ok':raise ValueError('incomplete OpenSTA: '+str(sta_dir))
                check_hashes({**result['sha256'],**result['receipts_sha256']})
                if row[tool]['netlist_sha256'] not in result['sha256'].values():raise ValueError('STA omits measured netlist')
                tr[tool]=result
                print('Setup Fmax:',did,tool,result['fmax_mhz'],flush=True)
        if a.phase in ('proofs','all'):proofs['rows'].append(pr);run.save(out/'proofs.json',proofs)
        if a.phase in ('timing','all'):timing['rows'].append(tr);run.save(out/'timing.json',timing)
    run.save(out/('complete-'+a.phase+'.json'),{'status':'complete','measurements_sha256':run.digest(ws/'results.json')})


if __name__=='__main__':main()
