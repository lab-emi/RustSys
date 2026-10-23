#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Publish a complete optional-PDK campaign alongside the frozen baseline.

<=15 comparison rows, <512 MiB reporting RSS. Stream hashes and evidence TAR;
<=65536 files, <=256 MiB copied receipt bytes, <=32 KiB/model. Larger models
stay in the hash inventory. Run under a 2 GiB / 600 s OS guard, one worker.
"""
from pathlib import Path
import argparse
import gzip
import math
import os
import tarfile

import additional_verify
import archive_json
import observed_proof
import functional_models
import run
import scaling_report
import scaling_verify

LABEL='EZ130 8T'
LIBRARY='ez130_8t_tt'


def new_rows(data, proofs, timing, spec):
    rows=additional_verify.measurement_rows(data,LIBRARY)
    p={r['id']:r for r in proofs['rows']};s={r['id']:r for r in timing['rows']}
    if len(proofs['rows'])!=5 or len(timing['rows'])!=5 or set(p)!=set(rows) or set(s)!=set(rows):
        raise ValueError('additional proof/timing matrix is incomplete or duplicated')
    output={}
    for did,row in rows.items():
        if p[did]['library']!=LIBRARY or s[did]['library']!=LIBRARY:
            raise ValueError('proof/timing PDK differs')
        for tool in ('rustsys','yosys'):
            proof=p[did][tool];sta=s[did][tool];digest=row[tool]['netlist_sha256']
            if (not scaling_verify.complete(proof) or sta['status']!='ok'
                    or digest not in proof.get('source_sha256',{}).values()
                    or digest not in sta.get('sha256',{}).values()
                    or spec['sha256'] not in proof.get('source_sha256',{}).values()
                    or spec['sha256'] not in sta.get('sha256',{}).values()):
                raise ValueError('incomplete or unbound additional LEC/STA')
            if any(type(v) not in (int,float) or not math.isfinite(v) or v<=0 for v in
                   (row[tool]['median_wall_s'],row[tool]['qor']['area_um2'],sta['fmax_mhz'])):
                raise ValueError('invalid measured time, area or Fmax')
        values=[row['design'],LABEL,*[f"{row[t]['median_wall_s']:,.2f}" for t in ('rustsys','yosys')],
                f"**{row['yosys']['median_wall_s']/row['rustsys']['median_wall_s']:.2f}×**"]
        values += [scaling_report.peak_memory_mib(row[t]) for t in ('rustsys','yosys')]
        values += [f"{row[t]['qor']['area_um2']:,.0f}" for t in ('rustsys','yosys')]
        values += [scaling_report.frequency(s[did][t]['fmax_mhz']) for t in ('rustsys','yosys')]
        output[did]='| '+' | '.join(values)+' |'
    return output


def combined_table(baseline, extra):
    data,proofs,timing=baseline
    lines=scaling_report.comparison_table(data,proofs,timing,include_quality=True).splitlines()
    output=lines[:2]
    for index,did in enumerate(scaling_report.ORDER):
        output += lines[2+2*index:4+2*index]+[extra[did]]
    return '\n'.join(output)


def archive_evidence(output, workspace, validation, controls):
    files=[];omitted=[];count=total=0
    suffixes={'.json','.py','.ys','.eqy','.tcl','.time','.stdout','.stderr','.md','.v','.aig','.aag'}
    for base,prefix in ((workspace,'measurements'),(validation,'validation'),(controls,'controls')):
        for path in base.rglob('*'):
            if path.is_symlink():raise ValueError('evidence symlink')
            if not path.is_file() or '__pycache__' in path.parts:continue
            count+=1
            if count>65536:raise ValueError('evidence file-count budget')
            size=path.stat().st_size
            limit=32768 if path.suffix in {'.aig','.aag','.v','.lib'} else 1024**2
            if path.suffix not in suffixes or size>limit:
                omitted.append({'path':str(path),'bytes':size,'sha256':run.digest(path)})
                continue
            total+=size
            if total>256*1024**2:raise ValueError('evidence copied-byte budget')
            files.append((path,Path(prefix)/path.relative_to(base)))
    for path in run.HERE.iterdir():
        if not path.is_file() or path.suffix not in {'.py','.json','.v'}:continue
        count+=1;total+=path.stat().st_size
        if count>65536 or path.stat().st_size>1024**2 or total>256*1024**2:
            raise ValueError('publication source snapshot budget')
        files.append((path,Path('harness-at-publication')/path.name))
    with (output/'evidence.tar.gz').open('wb') as stream:
        with gzip.GzipFile(fileobj=stream,mode='wb',filename='',mtime=0) as zipped:
            with tarfile.open(fileobj=zipped,mode='w|') as tar:
                for source,name in sorted(files,key=lambda item:str(item[1])):
                    info=tar.gettarinfo(str(source),arcname=str(name))
                    info.uid=info.gid=0;info.uname=info.gname='';info.mtime=0
                    with source.open('rb') as content:tar.addfile(info,content)
    evidence=scaling_report.split_large(output/'evidence.tar.gz')
    inventory=scaling_report.packed_json(output/'large-artifacts.json.gz',omitted)
    return evidence,inventory,{'evidence_raw_bytes':total,'evidence_files':len(files),'large_artifacts':len(omitted)}


def main():
    p=argparse.ArgumentParser(description=__doc__)
    for name in ('workspace','validation','controls','output'):p.add_argument('--'+name,type=Path,required=True)
    p.add_argument('--baseline',type=Path,default=run.HERE/'results/2026-10-21-default-multicore')
    p.add_argument('--state-controls',type=Path,help='external anonymous/encoded-state control receipt')
    p.add_argument('--update-docs',action='store_true')
    a=p.parse_args();ws=a.workspace.resolve();validation=a.validation.resolve();controls=a.controls.resolve()
    spec=run.read_json(run.HERE/'additional-pdks.json')['library'][0]
    data=run.read_json(ws/'results.json');proofs=run.read_json(validation/'proofs.json');timing=run.read_json(validation/'timing.json')
    extra=new_rows(data,proofs,timing,spec)
    additional_verify.check_hashes({**data['binary_sha256'],**data['input_sha256']})
    for reference in data['reference_executables']:additional_verify.check_hashes({reference['path']:reference['sha256']})
    for row in data['rows']:
        for tool in ('rustsys','yosys'):
            cfg=additional_verify.checked_config(ws,row,tool)
            if tool=='yosys':
                if (cfg.get('yosys_flow')!='arbitrary-initial-state-full-map-v3' or cfg.get('yosys_bounded_map') is not False
                        or cfg.get('yosys_abc_fast') is not False or cfg.get('yosys_delay_ff_optimization') is not True):
                    raise ValueError('reference flow differs from the baseline contract')
                script=(Path(cfg['directory'])/'synth.ys').read_text()
                if ('\nabc -liberty ' not in script or '\nsetundef -zero\n' not in script
                        or 'opt_dff' in script.split('\nsetundef -zero\n',1)[0]
                        or 'abc -fast' in script or '-script ' in script or 'setundef -init' in script):
                    raise ValueError('reference commands differ from the baseline contract')
    for row in proofs['rows']:
        for tool in ('rustsys','yosys'):
            proof=row[tool];additional_verify.check_hashes(proof['source_sha256'])
            supplement=proof.get('functional_library_supplement')
            if supplement:
                if run.digest(supplement['certificate'])!=supplement['certificate_sha256']:
                    raise ValueError('functional-model certificate changed')
                functional_models.validate(run.read_json(Path(supplement['certificate'])))
            if proof['method']==observed_proof.METHOD:observed_proof.validate(proof)
            for record in proof.get('tools',{}).values():
                if 'path' in record and 'sha256' in record:additional_verify.check_hashes({record['path']:record['sha256']})
            comparison=proof.get('certificate',{}).get('comparison',{})
            additional_verify.check_hashes(comparison.get('source_sha256',{}))
            additional_verify.check_hashes(comparison.get('tool_sha256',{}))
    for row in timing['rows']:
        for tool in ('rustsys','yosys'):
            additional_verify.check_hashes({**row[tool]['sha256'],**row[tool]['receipts_sha256']})
    checked={}
    for name,count in (('controls',3),('profile-controls',2)):
        record=run.read_json(controls/(name+'.json'))
        if record.get('status')!='passed' or len(record['rows'])!=count:raise ValueError('additional-library controls incomplete')
        for row in record['rows']:
            additional_verify.check_hashes(row.get('sha256',{}))
            if row.get('status')!='passed':raise ValueError('failed additional-library control')
        if name=='controls':
            cases={r['case']:r for r in record['rows']}
            if set(cases)!={'correct','bad-data','bad-clock'}:raise ValueError('missing corruption control')
            if any(cases[k]['accepted_as_equivalent']!=(k=='correct') for k in cases):
                raise ValueError('corruption control accepted an inequivalent netlist')
            for row in record['rows']:
                additional_verify.check_hashes(row['result'].get('source_sha256',{}))
                if row['library']!=Path(spec['filename']).stem:raise ValueError('corruption-control library differs')
                if row['case']=='correct':
                    if not scaling_verify.complete(row['result']):raise ValueError('positive control lacks a complete external proof')
                elif row['result'].get('certificate',{}).get('equivalent') is not False:
                    raise ValueError('negative control lacks external inequivalence')
        else:
            profiles={r['profile']:r for r in record['rows']}
            if set(profiles)!={'v1','v3'} or profiles['v3']['gate_states']!=16 or profiles['v3']['rtl_states']!=16:
                raise ValueError('arbitrary-state profile calibration is incomplete')
            if any(r['library']!=spec['filename'] or r['reset_assumptions'] or r['initial_state_choices'] for r in record['rows']):
                raise ValueError('profile calibration changed the library or initial-state scope')
        checked[name]=record
    proposals=run.read_json(a.state_controls or controls/'state-proposals/controls.json')
    expected={'correct','bad-data','bad-clock','encoded-correct','encoded-bad-phase','encoded-bad-reset'}
    if (proposals.get('status')!='passed' or len(proposals['rows'])!=len(expected)
            or {r['case'] for r in proposals['rows']}!=expected):
        raise ValueError('bounded state-proposal external controls incomplete')
    for row in proposals['rows']:
        additional_verify.check_hashes(row['sha256'])
        if row['status']!='passed' or row['accepted_as_equivalent']!=(row['case'] in {'correct','encoded-correct'}):
            raise ValueError('state proposal accepted a corrupted implementation/relation')
    checked['state-proposal-controls']=proposals
    baseline=tuple(archive_json.read(a.baseline/(name+'.json')) for name in ('measurements','proofs','timing'))
    if ({Path(path).name:h for path,h in data['binary_sha256'].items()}
            !={Path(path).name:h for path,h in baseline[0]['binary_sha256'].items()}):
        raise ValueError('additional campaign must use the exact baseline producers')
    sta_pins={h for row in baseline[2]['rows'] for tool in ('rustsys','yosys')
              for path,h in row[tool]['sha256'].items() if Path(path).name=='sta'}
    if len(sta_pins)!=1:raise ValueError('baseline OpenSTA executable is not uniquely pinned')
    for row in timing['rows']:
        for tool in ('rustsys','yosys'):
            current={h for path,h in row[tool]['sha256'].items() if Path(path).name=='sta'}
            if current!=sta_pins:raise ValueError('additional OpenSTA differs from the baseline executable')
    table=combined_table(baseline,extra)
    output=a.output.resolve();output.mkdir(parents=True,exist_ok=False)
    artifacts={name:scaling_report.packed_json(output/(name+'.json.gz'),value) for name,value in
        (('measurements',data),('proofs',proofs),('timing',timing),('controls',checked))}
    evidence,inventory,stats=archive_evidence(output,ws,validation,controls)
    cpu=next(line.split(':',1)[1].strip() for line in Path('/proc/cpuinfo').read_text().splitlines() if line.startswith('model name'))
    if cpu!=run.read_json(a.baseline/'provenance.json')['host']['cpu']:
        raise ValueError('combined table CPU differs from the baseline host')
    provenance={'host':{'cpu':cpu,'kernel':os.uname().release,'exclusive_host':False},'library':spec,
                'reporter_sha256':run.digest(Path(__file__)),
                'source_workspace':str(ws),'validation_workspace':str(validation),**stats,
                'scope':'five complete pinned designs; both tools at 16 physical cores; one warm-up plus three measured runs; fresh arbitrary-state LEC and OpenSTA',
                'baseline':str(a.baseline),'baseline_sha256':{path.name:run.digest(path) for path in a.baseline.iterdir() if path.is_file()},
                'publication_limits':{'max_files':65536,'max_evidence_bytes':256*1024**2,'max_model_bytes':32768}}
    provenance['functional_model_scope']=run.read_json(run.HERE/'ez130-functional-models.json')['scope']
    run.save(output/'provenance.json',provenance)
    links=' · '.join(f'[{name.title()}]({path})' for name,path in artifacts.items())
    content=f'''# IHP SG13G2 / EZ130 8T synthesis benchmark

**CPU: {cpu}.** Both tools use 16 physical host cores, without SMT siblings.
One warm-up and three measured runs per tool/design; reported time is the median.
The exact same RustSys and Yosys executables and RTL as the baseline campaign are used.
Time includes workspace init/import, synthesis, mmap and full exported Verilog.
Preparation, LEC and OpenSTA are outside the timer. Timed cases run serially on a shared host.
Peak mem is the maximum GNU-time process RSS across the three measurements; aggregate cgroup peaks remain in the receipts.

{chr(10).join(table.splitlines()[:2]+[extra[did] for did in scaling_report.ORDER])}

All ten new netlists pass fresh arbitrary-state RTL LEC with no reset or input assumptions.
Every implementation initial state is covered by checked correspondence; ordinary X/Z data
and undriven wires use the same explicit zero completion as the baseline, without initializing registers.
Independent RTL event obligations are checked by external ABC; inferred arithmetic also uses Arisca.
Anonymous states use bounded one-to-one proposals, with every original state still free;
only a complete external event/output proof accepts the relation.

**Cell-model scope:** EZ130 v1.1 marks TIEHI/TIELO with `open_drain`/`open_source`
driver types that contradict the same release's constant-output Verilog models.
Functional LEC uses those pinned vendor Verilog constants, independently checked
against an explicit two-attribute functional Liberty supplement with positive
and corrupted-constant ABC controls. The original synthesis/timing library and
all measured netlists are retained unchanged. Electrical drive strengths and
high-impedance behavior are outside this functional proof; the conflict and
exact byte edits are included in the receipts. General tri-state/strength cells
remain rejected by the native checker.

OpenC910 retains the complete ct_core with default 64 KiB instruction/data caches lowered to cells.

† OpenSTA setup-Fmax at TT 1.20 V / 25°C, ideal clocks, 100 ps slew, 10 fF port loads,
zero I/O delays and no wire parasitics. Same-clock/same-edge register paths only;
other and unconstrained paths are outside this estimate. Large lowered memories
can exceed Liberty load characterization; this does not establish achievable or signoff Fmax.

Library: [EZ130 8T v1.1]({spec['source']}), Apache-2.0, IHP SG13G2 130 nm.
Liberty SHA-256: `{spec['sha256']}`. Download/archive/member checksums are pinned
in [additional-pdks.json](../../additional-pdks.json).
Please cite Castañeda et al., [An Open-Source Standard-Cell Library for IHP 130 nm Developed by Students]({spec['citation']}).

{links} · [Raw receipts and measured harness]({evidence}) · [Large artifact inventory]({inventory}) · [Provenance](provenance.json).
Large models remain in the local workspace with streamed SHA-256 inventory.
Archive parts are at most 1 MiB; concatenate index-listed parts before decompressing.
'''
    (output/'README.md').write_text(content)
    (output/'SHA256SUMS').write_text(''.join(f'{run.digest(path)}  {path.name}\n' for path in sorted(output.iterdir()) if path.is_file() and path.name!='SHA256SUMS'))
    if a.update_docs:
        for name,start,end in [('README.md',scaling_report.START,scaling_report.END),
                               ('docs/index.md','<!-- benchmark-16-core:start -->','<!-- benchmark-16-core:end -->'),
                               ('docs/guide/benchmarks.md','<!-- benchmark-16-core:start -->','<!-- benchmark-16-core:end -->')]:
            path=run.ROOT/name
            path.write_text(scaling_report.block(path.read_text(),start,end,table))
    print('Published complete additional-library campaign:',output)


if __name__=='__main__':main()
