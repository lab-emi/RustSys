#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Bind a critical-path load observation to existing OpenSTA and Liberty data.

Two <=16MiB path receipts and one <=64MiB library; <512MiB planned RAM.
No path enumeration: inspect only the bounded path already reported by OpenSTA.
Accepts quoted or bare Liberty identifiers and an explicit 1pF load unit.
At most 8192 cells, 1024 pins/cell and 65536 reported path nodes.
"""
from pathlib import Path
import argparse
from itertools import islice
import re

import run


def groups(kind,text,limit):
    pattern=rf'\b{kind}\s*\(\s*(?:"([^"\n]+)"|([a-zA-Z_][a-zA-Z0-9_$]*))\s*\)\s*\{{'
    found=list(islice(re.finditer(pattern,text),limit+1))
    if len(found)>limit:raise ValueError('Liberty observation group budget')
    return found


def name(match):return match[1] or match[2]


def observe(timing,tool):
    path=next(Path(p) for p in timing['receipts_sha256'] if p.endswith('worst-path.json'))
    library=next(Path(p) for p in timing['sha256'] if p.endswith('.lib'))
    if path.stat().st_size>16*1024**2 or library.stat().st_size>64*1024**2:
        raise ValueError('electrical observation byte budget')
    pins={str(p):run.digest(p) for p in (path,library)}
    known={**timing['sha256'],**timing['receipts_sha256']}
    if any(known.get(p)!=h for p,h in pins.items()):raise ValueError('timing input changed')
    checks=[c for c in run.read_json(path)['checks'] if c.get('path_group')=='bench_clk'
        and c.get('source_clock')==c.get('target_clock')
        and c.get('source_clock_edge')==c.get('target_clock_edge')]
    if len(checks)!=1 or not checks[0]['source_path'][0].get('instance'):
        raise ValueError('expected one reported register path')
    if len(checks[0]['source_path'])>65536:raise ValueError('reported path-node budget')
    previous=0.0;steps=[]
    for node in checks[0]['source_path']:
        delay=node['arrival']-previous;previous=node['arrival']
        if node.get('instance') and 'capacitance' in node:steps.append((delay,node))
    delay,node=max(steps,key=lambda p:p[0]);cell=node['cell'];pin=node['pin'].rsplit('/',1)[-1]
    text=library.read_text()
    if not re.search(r'capacitive_load_unit\s*\(\s*1(?:\.0+)?\s*,\s*(?:"pf"|pf)\s*\)',text):
        raise ValueError('unsupported load unit')
    heads=groups('cell',text,8192)
    index=next(i for i,h in enumerate(heads) if name(h)==cell)
    body=text[heads[index].end():heads[index+1].start() if index+1<len(heads) else len(text)]
    ports=groups('pin',body,1024)
    index=next(i for i,p in enumerate(ports) if name(p)==pin)
    body=body[ports[index].end():ports[index+1].start() if index+1<len(ports) else len(body)]
    limit=float(re.search(r'\bmax_capacitance\s*:\s*([0-9.eE+-]+)\s*;',body)[1])
    tables=re.findall(r'\bcell_(?:rise|fall)\s*\([^)]*\)\s*\{([^{}]*)\}',body)
    endpoints=[float(re.search(r'\bindex_2\s*\(\s*"([^"]+)"',table)[1].split(',')[-1]) for table in tables]
    if not endpoints:raise ValueError('delay-table load axis missing')
    return {'tool':tool,'cell':cell,'pin':pin,'net':node.get('net'),
        'selection':'largest reported cell delay on the worst same-clock same-edge register path',
        'cell_delay_ns':delay*1e9,'load_pf':node['capacitance']*1e12,
        'cell_max_capacitance_pf':limit,'largest_delay_table_load_pf':max(endpoints),
        'outside_every_delay_table_load_axis':node['capacitance']*1e12>max(endpoints),
        'sha256':pins}


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--validation',type=Path,required=True)
    p.add_argument('--library',default='sky130hd_tt')
    p.add_argument('--record-any-load',action='store_true',help='retain the selected load observations even when not both out of range')
    a=p.parse_args();data=run.read_json(a.validation/'timing.json')
    row=next(r for r in data['rows'] if r['id']=='openc910' and r['library']==a.library)
    result={'schema':1,'library':a.library,'rows':[observe(row[t],t) for t in ('rustsys','yosys')]}
    if not a.record_any_load and not all(r['outside_every_delay_table_load_axis'] for r in result['rows']):
        raise ValueError('the requested out-of-range observation does not hold')
    run.save(a.validation/'openc910-electrical-scope.json',result)
    for r in result['rows']:print(r['tool'],r['cell'],r['load_pf'],r['cell_max_capacitance_pf'])
