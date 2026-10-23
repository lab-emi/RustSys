#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Explicit, externally checked EZ130 constant-cell functional interpretation.

Pinned <=16 MiB ZIP, <=32 MiB Liberty, <=1 MiB upstream Verilog. One worker,
<256 MiB parent and <1 GiB external frontend/CEC RSS expected. 2 GiB / 120 s
guards per child. Only two byte-logged driver_type edits are allowed; source
RTL, measured netlists, timing libraries and every initial state stay intact.
"""
from pathlib import Path
import hashlib
import re
import urllib.request
import zipfile

import equiv
import run
import transition


def validate(record):
    if record.get('status')!='certified' or len(record.get('checks',[]))!=4:
        raise ValueError('uncertified functional-model supplement')
    for path,digest in record['sha256'].items():
        if run.digest(path)!=digest:raise ValueError('functional-model source/receipt changed: '+path)
    for check in record['checks']:
        text=Path(check['stdout']).read_text()
        yes=check['measurement']['status']=='ok' and transition.cec_equivalent(text)
        if check['expected_equivalent']:
            if not yes:raise ValueError('vendor constant-cell model did not prove')
        elif yes or check['measurement']['status']!='ok' or 'Networks are NOT EQUIVALENT.' not in text:
            raise ValueError('constant-cell fault lacks external inequivalence')
    return record


def prepare(library, directory, yosys):
    library=Path(library).resolve();d=Path(directory).resolve()
    policy=run.read_json(run.HERE/'ez130-functional-models.json')
    if run.digest(library)!=policy['library_sha256']:return None
    if (d/'certificate.json').exists():return validate(run.read_json(d/'certificate.json'))
    d.mkdir(parents=True,exist_ok=False)
    spec=run.read_json(run.HERE/'additional-pdks.json')['library'][0]
    archive=run.ROOT/'.work/ez130-20261022/library/ez130_8t_v1p1.zip'
    if archive.exists():raw=archive.read_bytes()
    else:
        with urllib.request.urlopen(spec['archive_url'],timeout=30) as response:raw=response.read(16*1024**2+1)
    if len(raw)>16*1024**2 or hashlib.sha256(raw).hexdigest()!=policy['archive_sha256']:
        raise ValueError('vendor functional-model archive checksum/budget')
    import io
    with zipfile.ZipFile(io.BytesIO(raw)) as zipped:
        entry=zipped.getinfo(policy['member'])
        if entry.file_size!=policy['member_bytes'] or entry.file_size>1024**2:raise ValueError('vendor model byte budget')
        upstream=zipped.read(entry)
    if hashlib.sha256(upstream).hexdigest()!=policy['member_sha256']:raise ValueError('vendor model checksum differs')
    vendor=d/'upstream-ez130_8t.v';vendor.write_bytes(upstream)
    if library.stat().st_size>32*1024**2:raise ValueError('functional Liberty byte budget')
    original=library.read_bytes();heads=list(re.finditer(rb'\bcell\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)\s*\{',original))
    if len(heads)>8192:raise ValueError('functional Liberty cell-count budget')
    edits=[];models=[]
    for cell in policy['cells']:
        index=next(i for i,head in enumerate(heads) if head[1].decode()==cell['name'])
        start=heads[index].start();end=heads[index+1].start() if index+1<len(heads) else len(original)
        body=original[start:end]
        if not re.search(rb'function\s*:\s*"'+str(cell['constant']).encode()+rb'"\s*;',body):
            raise ValueError('unexpected tie-cell truth function')
        needle=('driver_type : '+cell['conflicting_driver_type']+';').encode()
        if body.count(needle)!=1:raise ValueError('tie-cell driver edit is not unique')
        offset=start+body.index(needle)
        edits.append({'cell':cell['name'],'pin':cell['pin'],'offset':offset,'original':needle.decode(),
                      'replacement':' '*len(needle),'reason':'conflicts with pinned nominal vendor Verilog constant model'})
        matches=list(re.finditer(rb'\bmodule\s+'+cell['name'].encode()+rb'\b.*?\bendmodule\b',upstream,re.S))
        if len(matches)!=1 or not re.search(rb'buf\s*\(\s*Y\s*,\s*1\x27b'+str(cell['constant']).encode()+rb'\s*\)',matches[0][0]):
            raise ValueError('vendor tie model differs from pinned constant behavior')
        models.append(matches[0][0])
    derived=bytearray(original)
    for edit in edits:
        offset=edit['offset'];data=edit['replacement'].encode();derived[offset:offset+len(data)]=data
    adapted=d/'ez130_8t_tt.functional.lib';adapted.write_bytes(derived)
    model=d/'tie-models.v';model.write_bytes(b'// SPDX-License-Identifier: Apache-2.0\n// Copyright 2026 ETH Zurich; exact modules from the pinned vendor model.\n'+b'\n\n'.join(models)+b'\n')
    abc=run.ROOT/'.work/lec-followup/oss-cad-20261014/oss-cad-suite/bin/yosys-abc'
    checks=[]
    def execute(argv,folder,label):
        result=run.execute(list(map(str,argv)),folder,label,seconds=120,memory_gib=2,max_file_bytes=1024**2)
        if result['status']!='ok':raise ValueError('functional-model preparation failed: '+str(folder/label))
        return result
    for cell in policy['cells']:
        folder=d/cell['name'];folder.mkdir();images={}
        for side,reader in [('vendor',f'read_verilog {equiv.quote(model)}'),
                            ('liberty',f'read_liberty -ignore_miss_func -ignore_miss_dir {equiv.quote(adapted)}')]:
            script=folder/(side+'.ys');aig=folder/(side+'.aig')
            script.write_text(reader+'\nhierarchy -check -top '+cell['name']+'\nproc\nflatten\ntechmap\nopt_clean\naigmap\nwrite_aiger '+equiv.quote(aig)+'\n')
            execute([yosys,'-Q','-T','-s',script],folder,side)
            with aig.open('rb') as stream:header=stream.readline(256).split()
            if header!=[b'aig',b'0',b'0',b'0',b'1',b'0']:raise ValueError('tie conformance must have zero inputs/states and one output')
            images[side]=aig
        mutant=folder/'opposite.aig';mutant.write_bytes(b'aig 0 0 0 1 0\n'+str(1-cell['constant']).encode()+b'\n')
        for kind,target in [('correct',images['liberty']),('bad-constant',mutant)]:
            command=transition.cec_command(transition.Limits(),120,images['vendor'],target)
            measured=execute([abc,'-c',command],folder,kind)
            checks.append({'cell':cell['name'],'case':kind,'expected_equivalent':kind=='correct',
                           'measurement':measured,'stdout':str(folder/(kind+'.stdout'))})
    pins=[library,vendor,adapted,model,run.HERE/'ez130-functional-models.json',Path(yosys),abc]
    pins += [p for p in d.rglob('*') if p.is_file() and p.suffix in {'.aig','.ys','.stdout','.stderr','.time','.json'}]
    record={'status':'certified','scope':policy['scope'],'original_library':str(library),
            'functional_library':str(adapted),'edits':edits,'checks':checks,
            'warning':{'code':'EZ130_TIE_DRIVER_CONFLICT','message':'TIEHI/TIELO driver_type contradicts vendor functional models; functional LEC uses the explicitly certified vendor constants. Electrical drive/strength behavior is outside this proof.'},
            'sha256':{str(path):run.digest(path) for path in pins}}
    validate(record);run.save(d/'certificate.json',record)
    print('WARNING[EZ130_TIE_DRIVER_CONFLICT]: using certified nominal vendor tie-cell models; electrical strengths remain outside functional LEC.',flush=True)
    return record


def bind(proof,record):
    if record is None:return proof
    validate(record)
    proof.setdefault('source_sha256',{}).update(record['sha256'])
    proof['functional_library_supplement']={'certificate':str(Path(record['functional_library']).parent/'certificate.json'),
        'certificate_sha256':run.digest(Path(record['functional_library']).parent/'certificate.json'),
        'scope':record['scope'],'warning':record['warning']}
    proof.setdefault('scope',{})['library_functional_model']=record['scope']
    return proof
