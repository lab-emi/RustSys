#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Check bounded pre/post optimization exports with opaque external ABC.

Planning: <=256 cases, <=100K ANDs/case (~16MiB graph), one solver with
2M-conflict/120s/1GiB RAM+512MiB swap guards. Reporter<64MiB. No reference
implementation is inspected. Export alone is never an equivalence result.
"""
from pathlib import Path
import argparse

import run
import slice_lec
import transition


def header(path):
    if path.stat().st_size>32*1024**2:raise ValueError('AIGER byte budget')
    with path.open('rb') as f:parts=f.readline(256).decode().split()
    if len(parts)!=6 or parts[0]!='aig':raise ValueError('expected combinational binary AIGER')
    m,i,l,o,a=map(int,parts[1:])
    if l or m!=i+a or a>100000 or i>100000 or not 0<o<=20000:raise ValueError('AIGER graph budget')
    return i,o


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--manifest',type=Path,required=True)
    p.add_argument('--directory',type=Path,required=True)
    p.add_argument('--abc',type=Path,required=True)
    p.add_argument('--cpu',type=int,default=20)
    a=p.parse_args()
    if a.manifest.stat().st_size>1024**2:raise ValueError('manifest byte budget')
    cases=run.read_json(a.manifest)
    if not 1<=len(cases)<=256:raise ValueError('case count budget')
    out=a.directory.resolve();out.mkdir(parents=True,exist_ok=False)
    abc=a.abc.resolve()
    tools={str(abc):run.digest(abc)}
    executable=abc.parent.parent/'libexec/yosys-abc'
    if executable.exists():tools[str(executable)]=run.digest(executable)
    rows=[]
    for index,case in enumerate(cases):
        gold,gate=Path(case['gold']).resolve(),Path(case['gate']).resolve()
        expected=(case['inputs'],case['roots'])
        if header(gold)!=expected or header(gate)!=expected:raise ValueError('export interface differs from manifest')
        pins={str(path):run.digest(path) for path in (gold,gate)}
        folder=out/f'case{index:04}';folder.mkdir()
        command=f'cec -n -T 120 -C 2000000 "{gold}" "{gate}";'
        result=slice_lec.bounded([abc,'-c',command],folder,'abc',125,a.cpu,1)
        yes=result['status']=='ok' and transition.cec_equivalent((folder/'abc.stdout').read_text())
        if any(run.digest(path)!=h for path,h in {**pins,**tools}.items()):raise ValueError('proof input/tool changed')
        rows.append({**case,'status':'proven' if yes else 'unverified','source_sha256':pins,'measurement':result,
                     'receipt_sha256':{str(path):run.digest(path) for path in folder.iterdir() if path.is_file()}})
        run.save(out/'results.json',{'status':'complete' if len(rows)==len(cases) and all(r['status']=='proven' for r in rows) else 'incomplete',
                 'method':'complete external ABC CEC over every pre/post output and state/event root',
                 'initial_states':'arbitrary graph inputs; no reset assumptions',
                 'tool_sha256':tools,'manifest_sha256':run.digest(a.manifest),'rows':rows})
        if not yes:raise ValueError('external pass CEC incomplete: '+str(folder))
        if len(rows)%16==0:print('Complete external pass proofs',len(rows),'/',len(cases),flush=True)
    print('All',len(rows),'external pass proofs complete',flush=True)


if __name__=='__main__':main()
