#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Untrusted EZ130 state hints from public pre/post-ABC Verilog exports.

This never proves equivalence. Complete independent event/output CEC must
validate every pair, including polarity. Matching uses scalar-Q cell order and
public name anchors; unused pre-map cells can be skipped within 64 records.
Streaming parser: <=2 GiB/file,64M lines,3.2M states,8192 hints,8 MiB names;
65-state lookahead and <256 MiB expected parent RSS, one worker. Optional
untimed capture is ~17 GiB observed /32 GiB guarded, serialized with scale.lock.
No reference-tool implementation source, reset or initial values are used.
"""
from pathlib import Path
from collections import deque
import argparse
import fcntl
import os
import re
import subprocess

import additional_verify
import equiv
import run

START=re.compile(r'^  (DFF\w*|LAT\w*)\s+(\S+)\s+\($')
QPIN=re.compile(r'^\s*\.Q\((.*?)\),?$')
ANONYMOUS=re.compile(r'^_\d+_$')
ASSIGN=re.compile(r'^  assign (.*?)\s+=\s+(~?)(.*?)\s*;\s*$')


def normalize(value):
    return re.sub(r'\s+','',value.replace('\\',''))


def lines(path):
    if path.stat().st_size>2*1024**3:raise ValueError('public Verilog byte budget')
    with path.open() as stream:
        for count,line in enumerate(stream):
            if count>=64000000 or len(line)>65536:raise ValueError('Verilog line/count budget')
            yield line


def states(path):
    cell=None;count=0;block=0
    for line in lines(path):
        if cell is None:
            if match:=START.match(line.rstrip()):cell,instance=match.groups();q=None;block=0
        else:
            block+=1
            if block>16:raise ValueError('scalar state cell block budget')
            if match:=QPIN.match(line.rstrip()):q=normalize(match[1])
            if line.strip()==');':
                count+=1
                if count>3200000 or q is None:raise ValueError('state/Q count budget')
                yield cell,instance,q
                cell=None
    if cell is not None:raise ValueError('unfinished scalar state cell')


def align(preabc,netlist):
    before,after=iter(states(preabc)),iter(states(netlist));aq,bq=deque(),deque()
    def fill(queue,source):
        while len(queue)<65:
            try:queue.append(next(source))
            except StopIteration:break
    fill(aq,before);fill(bq,after)
    pairs=[];removed=[];count=anchors=mismatches=0
    while aq and bq:
        a,b=aq[0],bq[0]
        if a[0]!=b[0] or (not ANONYMOUS.fullmatch(b[2]) and a[2]!=b[2]):
            skip=next((i for i,x in enumerate(aq) if i and x[0]==b[0]
                       and x[2]==b[2] and not ANONYMOUS.fullmatch(x[2])),None)
            if skip:
                for _ in range(skip):removed.append(aq.popleft())
                if len(removed)>8192:raise ValueError('pre-only state budget')
                fill(aq,before);continue
            if a[0]!=b[0]:raise ValueError('unaligned state types; no hint emitted')
        if ANONYMOUS.fullmatch(b[2]):
            pairs.append({'pre_cell':a,'post_cell':b,'state_order':count})
            if len(pairs)>8192:raise ValueError('anonymous hint count budget')
        elif a[2]==b[2]:anchors+=1
        else:mismatches+=1
        aq.popleft();bq.popleft();fill(aq,before);fill(bq,after);count+=1
        if count>3200000:raise ValueError('alignment state count budget')
    if aq or bq:raise ValueError('unaligned state trailer')
    if not count or not anchors:raise ValueError('no state/name anchors')
    return {'not_a_proof':True,'states':count,'anchors':anchors,'different_public_aliases':mismatches,
            'pre_only':removed,'anonymous':pairs}


def propose(preabc,alignment):
    wanted={row['pre_cell'][2]:[] for row in alignment['anonymous']};name_bytes=0
    for line in lines(preabc):
        if not line.startswith('  assign '):continue
        match=ASSIGN.match(line)
        if not match:continue
        rhs=normalize(match[3])
        if rhs not in wanted:continue
        lhs=normalize(match[1]);name_bytes+=len(lhs)
        if name_bytes>8*1024**2 or len(wanted[rhs])>=64:raise ValueError('alias/name budget')
        wanted[rhs].append({'rtl_state':lhs,'inverted':match[2]=='~'})
    hints=[];seen=set()
    for row in alignment['anonymous']:
        aliases=wanted[row['pre_cell'][2]]
        if len(aliases)!=1:raise ValueError('missing/ambiguous scalar public alias')
        hint={**aliases[0],'gate_instance':row['post_cell'][1]}
        if hint['rtl_state'] in seen:raise ValueError('duplicate proposed RTL variable')
        seen.add(hint['rtl_state']);hints.append(hint)
    return hints


def capture(config,directory):
    cfg=run.read_json(config);measured=Path(cfg['directory'])
    if cfg['design']['id']!='openc910' or cfg['tool']!='yosys':raise ValueError('capture requires measured Yosys ct_core config')
    original=measured/'synth.ys';text=original.read_text()
    if text.count('\nabc -liberty ')!=1:raise ValueError('unsupported public mapping script')
    script=directory/'capture.ys';preabc=directory/'preabc.v'
    prefix=text.split('\nabc -liberty ',1)[0]+'\n'
    prefix=prefix.replace('"priority-source-0.v"',equiv.quote(measured/'priority-source-0.v'))
    script.write_text(prefix+'write_verilog -noattr '+equiv.quote(preabc)+'\n')
    pins={str(p):run.digest(p) for p in (config,original,script,measured/'priority-source-0.v',Path(cfg['library']),Path(cfg['yosys']))}
    unit='rustsys-ez130-state-metadata-'+str(os.getpid())+'.scope'
    with (run.ROOT/'.work/scale.lock').open('a') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX)
        available=next(int(s.split()[1]) for s in Path('/proc/meminfo').read_text().splitlines() if s.startswith('MemAvailable:'))
        if available<34*1024**2:raise ValueError('insufficient available RAM for metadata capture')
        try:
            measured_run=run.execute(['systemd-run','--user','--scope','--quiet','--unit='+unit,
                '-p','MemoryMax=32G','-p','MemorySwapMax=1G','taskset','-c','16-31',cfg['yosys'],
                '-Q','-T','-s',str(script)],directory,'capture',seconds=2400,memory_gib=40,max_file_bytes=2*1024**3)
        finally:subprocess.run(['systemctl','--user','kill','--signal=KILL',unit],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    additional_verify.check_hashes(pins)
    run.save(directory/'capture.json',{'not_a_proof':True,'measurement':measured_run,'source_sha256':pins})
    if measured_run['status']!='ok':raise ValueError('public metadata capture incomplete')
    return preabc


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config',type=Path,required=True)
    parser.add_argument('--preabc',type=Path,help='reuse an already captured public Verilog file')
    parser.add_argument('--directory',type=Path,required=True)
    args=parser.parse_args();out=args.directory.resolve();out.mkdir(parents=True,exist_ok=False)
    config=args.config.resolve();cfg=run.read_json(config);netlist=Path(cfg['directory'])/'netlist.v'
    preabc=args.preabc.resolve() if args.preabc else capture(config,out)
    pins={str(p):run.digest(p) for p in (config,preabc,netlist,Path(__file__).resolve())}
    aligned=align(preabc,netlist);hints=propose(preabc,aligned)
    additional_verify.check_hashes(pins)
    run.save(out/'alignment.json',aligned);run.save(out/'proposals.json',hints)
    run.save(out/'metadata.json',{'not_a_proof':True,'source_sha256':pins,'hint_count':len(hints),
                                'proposals_sha256':run.digest(out/'proposals.json')})
    print('Untrusted state proposals:',len(hints),'Full external CEC is still required.',flush=True)


if __name__=='__main__':main()
