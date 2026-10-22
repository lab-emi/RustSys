#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Explicit synthesis-quality profiles and optional CLI-stage CPU accounting.

Same four daemonless workspace commands as the common baseline. Parent scaling
runner supplies affinity, physical/virtual RAM, file-size and wall guards.
Reporter planning<64MiB: one <=1MiB config, four command/CPU records. No netlist
or graph is loaded here. All command startup/import/mmap/export remains timed.
"""
from pathlib import Path
import resource
import shutil
import subprocess
import sys
import time

import run


def pipeline(path):
    path=Path(path)
    if path.stat().st_size>1024**2:raise ValueError('profile config byte budget')
    c=run.read_json(path);d=c['design'];directory=Path(c['directory'])
    if c['tool']!='rustsys' or not 1<=len(d['files'])<=4096:raise ValueError('invalid profile interface')
    ws=directory/'ws'
    common=['--workspace',str(ws),'--threads',str(c['threads']),'--format','json']
    synth=[c['rustsys'],*common,'--progress','off','synth','--base-rev','rev1',
           '--backend','cpu','--effort',c.get('rustsys_effort','medium'),'--goal',c.get('rustsys_goal','area')]
    for key,flag in (('timing_period','--timing-period'),('area_limit','--area-limit'),('buffer_fanout','--buffer-fanout')):
        if key=='buffer_fanout' and c.get('timing_period') is None:continue
        if c.get(key) is not None:synth.extend([flag,str(c[key])])
    commands=[
        [c['rusteda'],*common,'init'],
        [c['rusteda'],*common,'import','--base-rev','rev0','--lib',c['library'],
         '--corner',c['corner'],'--top',d['top'],'--rtl',*d['files']],
        synth,
        [c['rusteda'],*common,'export','--rev','rev2','--netlist',str(directory/'netlist.v')],
    ]
    run.save(directory/'pipeline.json',commands)
    rows=[]
    for name,command in zip(('init','import','synth','export'),commands):
        before=resource.getrusage(resource.RUSAGE_CHILDREN);started=time.monotonic()
        code=subprocess.call(command,stdin=subprocess.DEVNULL)
        wall=time.monotonic()-started;after=resource.getrusage(resource.RUSAGE_CHILDREN)
        if c.get('profile_commands'):
            cpu=after.ru_utime+after.ru_stime-before.ru_utime-before.ru_stime
            rows.append({'stage':name,'wall_s':wall,'cpu_s':cpu,'average_active_cores':cpu/wall,'exit_code':code})
            run.save(directory/'stage-timing.json',{'scope':'complete CLI commands, including startup and workspace I/O','rows':rows})
        if code:return code
    reports=list(ws.glob('revisions/*/synth.json'))
    if len(reports)!=1:raise ValueError('expected one synthesis report')
    shutil.copyfile(reports[0],directory/'report.json')
    return 0


if __name__=='__main__':
    if len(sys.argv)!=2:raise SystemExit('usage: rustsys_profile.py CONFIG.json')
    raise SystemExit(pipeline(sys.argv[1]))
