#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Exact-byte reuse of complete, locally retained proof and timing receipts.

At most8 JSON files/64MiB source bytes, <512MiB estimated decoded records.
All file hashing streams; no design graph or solver is constructed here.
Reuse is explicit provenance, never represented as a fresh solver invocation.
"""
from pathlib import Path
import copy
import math

import observed_proof
import run


def load(paths, kind):
    if len(paths)>8 or sum(Path(p).stat().st_size for p in paths)>64*1024**2:
        raise ValueError('reusable evidence input budget')
    result={}
    for name in paths:
        path=Path(name).resolve();data=run.read_json(path);receipt_hash=run.digest(path)
        rows=data.get('rows',[])
        if len(rows)>40:raise ValueError('reusable evidence row budget')
        for row in rows:
            if 'id' not in row:continue
            for tool in ('rustsys','yosys'):
                value=row.get(kind) if row.get('tool')==tool else row.get(tool)
                if isinstance(value,dict):
                    result.setdefault((row['id'],row['library'],tool),[]).append((path,receipt_hash,value))
    return result


def check_hashes(pins):
    for name,digest in pins.items():
        path=name.replace('$RUSTSYS_ROOT',str(run.ROOT))
        if run.digest(path)!=digest:raise ValueError('reused evidence changed: '+path)


def same_top(value,pins,digest,cfg):
    top=cfg.get('design',{}).get('top')
    if not isinstance(top,str):return False
    recorded=value.get('scope',{}).get('design_top')
    if recorded is not None:return recorded==top
    found=False
    for name,h in pins.items():
        path=Path(name)
        if h!=digest or path.name!='netlist.v':continue
        config=path.parent/'config.json'
        if not config.is_file():continue
        if config.stat().st_size>1024**2:raise ValueError('prior config byte budget')
        if run.read_json(config).get('design',{}).get('top')!=top:return False
        found=True
    return found


def proof(index,key,cfg,netlist,complete):
    digest=run.digest(netlist)
    inputs={str(Path(f).resolve()):run.digest(f) for f in [cfg['library'],*cfg['design']['files']]}
    for path,receipt_hash,value in index.get(key,[]):
        pins={p.replace('$RUSTSYS_ROOT',str(run.ROOT)):h for p,h in value.get('source_sha256',{}).items()}
        if digest not in pins.values() or any(pins.get(p)!=h for p,h in inputs.items()):continue
        if not same_top(value,pins,digest,cfg):continue
        if not complete(value):raise ValueError('matching prior proof is incomplete')
        check_hashes(pins)
        check_hashes(value.get('artifact_sha256',{}))
        for tool in value.get('tools',{}).values():
            if isinstance(tool,dict) and 'path' in tool and 'sha256' in tool:
                check_hashes({tool['path']:tool['sha256']})
        comparison=value.get('certificate',{}).get('comparison',{})
        check_hashes(comparison.get('source_sha256',{}))
        check_hashes(comparison.get('tool_sha256',{}))
        if value.get('method')==observed_proof.METHOD:observed_proof.validate(value)
        out=copy.deepcopy(value)
        out.setdefault('source_sha256',{})[str(netlist.resolve())]=digest
        out['reuse_binding']={'status':'reused_exact_bytes','netlist_sha256':digest,
            'source_and_library_sha256_checked':True,'source_receipt':str(path),
            'source_receipt_sha256':receipt_hash,'design_top_checked':cfg['design']['top'],
            'fresh_solver_run':False}
        return out
    return None


def timing(index,key,netlist,library,sta,clock):
    digest=run.digest(netlist);lib_digest=run.digest(library);sta_digest=run.digest(sta)
    for path,receipt_hash,value in index.get(key,[]):
        pins=value.get('sha256',{})
        if digest not in pins.values() or lib_digest not in pins.values() or sta_digest not in pins.values():continue
        expected={'clock':'bench_clk on '+clock,'input_transition_ns':0.1,'output_load_pf':0.01,
            'input_output_delays_ns':0,'root_clock_period_ns':1000,
            'clock_model':'ideal;100ps transition;zero uncertainty',
            'wire_model':'none; no estimated or routed parasitics'}
        if any(value.get('scope',{}).get(k)!=v for k,v in expected.items()):continue
        if (value.get('status')!='ok' or not isinstance(value.get('fmax_mhz'),(int,float))
                or not math.isfinite(value['fmax_mhz']) or value['fmax_mhz']<=0):
            raise ValueError('matching prior timing is incomplete')
        check_hashes(pins);check_hashes(value.get('receipts_sha256',{}))
        out=copy.deepcopy(value);out['sha256'][str(netlist.resolve())]=digest
        out['reuse_binding']={'status':'reused_exact_bytes','netlist_sha256':digest,
            'library_sha256':lib_digest,'source_receipt':str(path),
            'source_receipt_sha256':receipt_hash,'fresh_sta_run':False}
        return out
    return None
