#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Publish the complete common multicore campaign, never partial measurements.

Planning<1GiB reporter RSS:20 bounded row records, streamed hashes/archive.
Evidence scan<=65536 files by default, copied raw bytes<=256MiB. Receipt entries
are<=1MiB; reproducible model payloads default to32KiB. Larger models stay in
the SHA-256 inventory. At most~65MiB bounded path records; streamed archive,
one corner, one reporter. Use a2GiB/300s reporter OS guard.
No compiler, graph search, placement or new solver is run by publication.
"""
from pathlib import Path
import argparse
import copy
from datetime import datetime,timezone
import gzip
import hashlib
import json
import os
import tarfile

import run
import scaling_verify

START='<!-- open-hardware-benchmark:start -->'
END='<!-- open-hardware-benchmark:end -->'
LARGE_START='<!-- large-hardware-benchmark:start -->'
LARGE_END='<!-- large-hardware-benchmark:end -->'
SCALE_START='<!-- parallel-hardware-scaling:start -->'
SCALE_END='<!-- parallel-hardware-scaling:end -->'
ORDER=['ibex','picorv32','serv','vexriscv-verilog','openc910']
LIBS=['sky130hd_tt','gf180mcu7t_tt']
PDK={'sky130hd_tt':'Sky130 HD','gf180mcu7t_tt':'GF180 7T'}


def block(text,begin,end,body):
    if text.count(begin)!=1 or text.count(end)!=1:raise ValueError('expected one publication marker pair: '+begin)
    left,right=text.index(begin)+len(begin),text.index(end)
    if left>=right:raise ValueError('publication markers out of order')
    return text[:left]+'\n\n'+body+'\n\n'+text[right:]


def pair(values,formatter):return ' / '.join(formatter(values[t]) for t in ('rustsys','yosys'))


def frequency(value):
    # Preserve significant digits for fully lowered, high-fanout large designs.
    return f'{value:.3g}' if value<1 else f'{value:.2f}'


def tables(data,proofs,timing):
    rows={(r['id'],r['library'],r['allocated_physical_cores']):r for r in data['rows']}
    p={(r['id'],r['library']):r for r in proofs['rows']}
    s={(r['id'],r['library']):r for r in timing['rows']}
    if not scaling_verify.ready(data) or set(p)!=set(s) or len(p)!=10:raise ValueError('incomplete common campaign')
    unified=data.get('scope',{}).get('required_reference_cores')==[1,16]
    header='| Design | PDK | Time, s ↓<br>RustSys / Yosys | Speedup<br>Yosys ÷ RustSys | Peak RSS, MiB ↓<br>RustSys / Yosys | Cell area, µm² ↓<br>RustSys / Yosys | Raw setup Fmax‡, MHz ↑<br>RustSys / Yosys | RTL LEC<br>RustSys / Yosys |'
    main=[header,'|---|---|---:|---:|---:|---:|---:|---|']
    if unified:
        main=['| Design · PDK | Tool<br>RustSys vs Yosys | 1 core, s ↓ | 16 cores, s ↓ | 1→16 speedup | Active cores / 16 | Cell area, µm² ↓ | Setup Fmax‡, MHz ↑ | Arbitrary-state RTL LEC |',
              '|---|---|---:|---:|---:|---:|---:|---:|---|']
    scale=['| Design | PDK | RustSys 1 core, s ↓ | RustSys 16 cores, s ↓ | RustSys 1→16 scaling | Average active cores, of 16<br>RustSys / Yosys | RustSys netlist<br>1 vs 16 cores |',
           '|---|---|---:|---:|---:|---:|---|']
    speedups=[];selfups=[]
    for did in ORDER:
        for lib in LIBS:
            r=rows[did,lib,16];one=rows[did,lib,1];key=did,lib
            for t in ('rustsys','yosys'):
                if not scaling_verify.complete(p[key][t]) or s[key][t]['status']!='ok':raise ValueError('incomplete proof/timing result')
                if (r[t]['netlist_sha256'] not in p[key][t].get('source_sha256',{}).values()
                        and p[key][t].get('reuse_binding',{}).get('netlist_sha256')!=r[t]['netlist_sha256']):
                    raise ValueError('proof is not bound to the measured netlist')
                if len(r[t]['samples'])!=3:raise ValueError('exactly three measured repetitions required')
                if r[t]['netlist_sha256'] not in s[key][t]['sha256'].values():
                    raise ValueError('timing is not bound to the measured netlist')
                for result in r[t]['samples']:
                    if result['status']!='ok' or result['netlist_sha256']!=r[t]['netlist_sha256']:raise ValueError('nondeterministic or incomplete synthesis')
                if r[t].get('median_average_cpu_cores') is None:raise ValueError('missing measured CPU accounting')
            if r['rustsys']['netlist_sha256']!=one['rustsys']['netlist_sha256'] or r['rustsys']['qor']!=one['rustsys']['qor']:
                raise ValueError('thread count changed the implementation or area')
            speed=r['yosys']['median_wall_s']/r['rustsys']['median_wall_s']
            own=one['rustsys']['median_wall_s']/r['rustsys']['median_wall_s']
            name=r['design']
            labels={t:'Proven' for t in ('rustsys','yosys')}
            values=[name,PDK[lib],pair(r,lambda a:f"{a['median_wall_s']:.2f}"),f'{speed:.2f}×',
                    pair(r,lambda a:f"{a['peak_rss_kib']/1024:,.0f}"),pair(r,lambda a:f"{a['qor']['area_um2']:,.0f}"),
                    pair(s[key],lambda a:frequency(a['fmax_mhz'])),pair(labels,str)]
            if unified:
                for tool,label in (('rustsys','RustSys'),('yosys','Yosys')):
                    if (r[tool]['netlist_sha256']!=one[tool]['netlist_sha256'] or r[tool]['qor']!=one[tool]['qor']
                            or len(one[tool]['samples'])!=3 or any(x['status']!='ok'
                            or x['netlist_sha256']!=r[tool]['netlist_sha256'] for x in one[tool]['samples'])):
                        raise ValueError('one-core measurement lacks identical verified output')
                    result=r[tool]; first=one[tool]
                    values=[name+' · '+PDK[lib],label,f"{first['median_wall_s']:.2f}",f"{result['median_wall_s']:.2f}",
                        f"{first['median_wall_s']/result['median_wall_s']:.2f}×",f"{result['median_average_cpu_cores']:.2f}",
                        f"{result['qor']['area_um2']:,.0f}",
                        frequency(s[key][tool]['fmax_mhz']),'Proven · 1 & 16 cores']
                    main.append('| '+' | '.join(values)+' |')
            else:main.append('| '+' | '.join(values)+' |')
            scale.append('| '+' | '.join([name,PDK[lib],f"{one['rustsys']['median_wall_s']:.2f}",
                f"{r['rustsys']['median_wall_s']:.2f}",f'{own:.2f}×',pair(r,lambda a:f"{a['median_average_cpu_cores']:.2f}"),'Byte-identical'])+' |')
            speedups.append(speed);selfups.append(own)
    return '\n'.join(main),('' if unified else '\n'.join(scale)),speedups,selfups


def comparison_table(data,proofs,timing,*,include_quality=False):
    """A 16-core overview; retain the full campaign's proof and timing gates."""
    tables(data,proofs,timing)
    rows={(r['id'],r['library'],r['allocated_physical_cores']):r for r in data['rows']}
    columns=['Design','PDK','RustSys<br>16 cores, s ↓','Yosys<br>16 cores, s ↓','RustSys speedup ↑']
    alignment=['---','---','---:','---:','---:']
    if include_quality:
        columns[4]='RustSys<br>Speedup ↑'
        columns+=['RustSys<br>Area, µm² ↓','Yosys<br>Area, µm² ↓',
                  'RustSys<br>Setup Fmax†, MHz ↑','Yosys<br>Setup Fmax†, MHz ↑']
        alignment+=['---:']*4
    lines=['| '+' | '.join(columns)+' |','|'+'|'.join(alignment)+'|']
    estimates={(r['id'],r['library']):r for r in timing['rows']}
    for did in ORDER:
        for lib in LIBS:
            row=rows[did,lib,16]
            rustsys=row['rustsys']['median_wall_s'];yosys=row['yosys']['median_wall_s']
            values=[row['design'],PDK[lib],f'{rustsys:,.2f}',f'{yosys:,.2f}',f'**{yosys/rustsys:.2f}×**']
            if include_quality:
                values += [f"{row[t]['qor']['area_um2']:,.0f}" for t in ('rustsys','yosys')]
                values += [frequency(estimates[did,lib][t]['fmax_mhz']) for t in ('rustsys','yosys')]
            lines.append('| '+' | '.join(values)+' |')
    return '\n'.join(lines)


def split_large(path):
    if path.stat().st_size<=1024**2:return path.name
    parts=[];digest=run.digest(path)
    with path.open('rb') as content:
        for index in range(4096):
            chunk=content.read(1024**2)
            if not chunk:break
            part=path.with_name(path.name+f'.part-{index:04d}');part.write_bytes(chunk);parts.append(part.name)
        else:raise ValueError('compressed publication part-count budget reached')
    manifest=path.with_name(path.name+'.index.json')
    run.save(manifest,{'original_name':path.name,'sha256':digest,'parts':parts,'reassemble':'concatenate listed parts in order'})
    path.unlink()
    return manifest.name


def packed_json(path,value):
    raw=(json.dumps(value,indent=2,sort_keys=True)+'\n').encode()
    with path.open('wb') as stream:
        with gzip.GzipFile(fileobj=stream,mode='wb',filename='',mtime=0) as zipped:zipped.write(raw)
    return split_large(path)


def publish(a):
    max_files=getattr(a,'max_files',65536)
    max_model_bytes=getattr(a,'max_model_bytes',32768)
    max_evidence_bytes=getattr(a,'max_evidence_bytes',256*1024**2)
    if not 20000<=max_files<=262144 or not 0<=max_model_bytes<=1024**2 or not 64*1024**2<=max_evidence_bytes<=1024**3:
        raise ValueError('invalid bounded evidence publication limits')
    data=run.read_json(a.workspace/'results.json');proofs=run.read_json(a.validation/'proofs.json');timing=run.read_json(a.validation/'timing.json')
    unified=data.get('scope',{}).get('required_reference_cores')==[1,16]
    controls=run.read_json(a.controls/'controls.json')
    if controls.get('status')!='passed' or len(controls.get('rows',[]))!=6:raise ValueError('dual-PDK bridge controls incomplete')
    profile=run.read_json(a.controls/'initial-state-profile/profile-controls.json')
    if profile.get('status')!='passed' or len(profile.get('rows',[]))!=4:
        raise ValueError('dual-PDK arbitrary-state profile controls incomplete')
    for row in profile['rows']:
        for path,digest in row['sha256'].items():
            if run.digest(path)!=digest:raise ValueError('arbitrary-state control artifact changed')
    controls['initial_state_profile_controls']=profile
    observed=run.read_json(a.controls/'observed-method/calibration/controls.json')
    arithmetic=run.read_json(a.controls/'observed-method/calibration/arithmetic-controls.json')
    if observed.get('status')!='passed' or len(observed.get('rows',[]))!=6:
        raise ValueError('dual-PDK observed-method data/clock controls incomplete')
    if arithmetic.get('status')!='passed' or len(arithmetic.get('rows',[]))!=3:
        raise ValueError('signed modular arithmetic low/high-bit controls incomplete')
    for row in observed['rows']+arithmetic['rows']:
        for path,digest in row.get('sha256',{}).items():
            if run.digest(path)!=digest:raise ValueError('observed-method control artifact changed')
        if row.get('baseline_proof') and run.digest(row['baseline_proof'])!=row['baseline_sha256']:
            raise ValueError('observed-method positive baseline changed')
    controls['observed_method_controls']=observed
    controls['arithmetic_encoding_controls']=arithmetic
    main,scale,speeds,selfups=tables(data,proofs,timing)
    for row in data['rows']:
        for t in ('rustsys','yosys'):
            if t not in row:continue
            for rep in (1,2,3):
                d=a.workspace/row['id']/row['library']/f"{row['allocated_physical_cores']}c"/f'{t}-{rep}'
                if run.digest(d/'netlist.v')!=row[t]['netlist_sha256']:raise ValueError('measured netlist changed before publication')
                cfg=run.read_json(d/'config.json')
                if cfg.get('threads')!=row['allocated_physical_cores']:raise ValueError('measured CPU allocation differs from table')
                if t=='yosys':
                    if (cfg.get('yosys_flow')!='arbitrary-initial-state-full-map-v3'
                            or cfg.get('yosys_bounded_map') is not False or cfg.get('yosys_abc_fast') is not False
                            or cfg.get('yosys_delay_ff_optimization') is not True
                            or cfg.get('normalize_case_promises') is not True):
                        raise ValueError('all designs require the same initial-state-preserving/default-mapping reference profile')
                    script=(d/'synth.ys').read_text()
                    if ('\nabc -liberty ' not in script or '\nsetundef -zero\n' not in script
                            or 'opt_merge -keepdc' not in script or 'opt_dff -keepdc' not in script
                            or 'opt_dff' in script.split('\nsetundef -zero\n',1)[0]
                            or '-script ' in script or 'abc -fast' in script or 'setundef -init' in script):
                        raise ValueError('reference command script differs from the published common contract')
    for row in timing['rows']:
        for t in ('rustsys','yosys'):
            for path,digest in {**row[t]['sha256'],**row[t].get('receipts_sha256',{})}.items():
                if run.digest(path)!=digest:raise ValueError('timing inputs, constraints or executable changed')
    for path,digest in {**data['binary_sha256'],**data['input_sha256']}.items():
        if run.digest(path)!=digest:raise ValueError('measured input or executable changed')
    out=a.output.resolve();out.mkdir(parents=True,exist_ok=False)
    relative=out.relative_to(run.ROOT).as_posix()
    host={'cpu':next(s.split(':',1)[1].strip() for s in Path('/proc/cpuinfo').read_text().splitlines() if s.startswith('model name')),
          'kernel':os.uname().release,'metadata_collected':'at publication; per-invocation affinity and time receipts are retained',
          'exclusive_host':False}
    measurement_file=packed_json(out/'measurements.json.gz',data)
    proof_file=packed_json(out/'proofs.json.gz',proofs)
    timing_file=packed_json(out/'timing.json.gz',timing)
    control_file=packed_json(out/'controls.json.gz',controls)
    omitted=[];files=[];count=0;total=0
    suffixes={'.json','.py','.ys','.eqy','.tcl','.time','.stdout','.stderr','.md','.v','.aig','.aag'}
    for base,prefix in ((a.workspace,'measurements'),(a.validation,'validation'),(a.controls,'controls')):
        for path in base.rglob('*'):
            if path.is_symlink():raise ValueError('symlinks are not archived')
            if not path.is_file() or '__pycache__' in path.parts:continue
            count+=1
            if count>max_files:raise ValueError('publication file-count budget reached')
            size=path.stat().st_size
            limit=max_model_bytes if path.suffix in {'.aig','.aag','.v','.lib'} else 1024**2
            if path.suffix not in suffixes or size>limit:
                omitted.append({'path':str(path),'bytes':size,'sha256':run.digest(path),'reason':'large/reproducible artifact retained in measurement workspace'})
                continue
            total+=size
            if total>max_evidence_bytes:raise ValueError('publication raw-byte budget reached')
            files.append((path,Path(prefix)/path.relative_to(base)))
    def append_source(path,name):
        nonlocal total,count
        count+=1;size=path.stat().st_size;total+=size
        if count>max_files or size>1024**2 or total>max_evidence_bytes:
            raise ValueError('publication source snapshot budget reached')
        files.append((path,name))
    for path in sorted(run.HERE.glob('*.py')):append_source(path,Path('current-harness')/path.name)
    checker_sources={}
    checker_root=run.ROOT/'crates/verify/rustsys-check'
    for path in sorted(checker_root.rglob('*.rs')):
        if path.stat().st_size>1024**2:raise ValueError('checker source snapshot file budget')
        relative_source=path.relative_to(run.ROOT)
        checker_sources[relative_source.as_posix()]=run.digest(path)
        append_source(path,Path('checker-source-at-publication')/relative_source)
    with (out/'evidence.tar.gz').open('wb') as stream:
        with gzip.GzipFile(fileobj=stream,mode='wb',filename='',mtime=0) as zipped:
            with tarfile.open(fileobj=zipped,mode='w|') as tar:
                for source,name in sorted(files,key=lambda item:str(item[1])):
                    info=tar.gettarinfo(str(source),arcname=str(name));info.uid=info.gid=0;info.uname=info.gname='';info.mtime=0
                    with source.open('rb') as content:tar.addfile(info,content)
    evidence_file=split_large(out/'evidence.tar.gz')
    large_file=packed_json(out/'large-artifacts.json.gz',omitted)
    run.save(out/'provenance.json',{'host':host,'measurements_sha256':run.digest(a.workspace/'results.json'),
        'source_workspace':str(a.workspace),'validation_workspace':str(a.validation),
        'evidence_raw_bytes':total,'evidence_files':len(files),'large_artifacts':len(omitted),
        'checker_source_snapshot_sha256':checker_sources,
        'source_snapshot_scope':'checker source at publication; exact frozen compiler/checker executable hashes and per-run receipts remain authoritative',
        'publication_limits':{'max_files':max_files,'max_model_bytes':max_model_bytes,'max_evidence_bytes':max_evidence_bytes},
        'matrix':('RustSys and Yosys each at1/16physicalcores,5designs×2PDKs;1warmup+3measured runs;40allocations'
            if unified else 'RustSys1/16physical cores;Yosys16physical cores,5designs×2PDKs;1warmup+3measured runs'),
        'method':('every displayed allocation was actually measured; both tools1/16netlist hashes andQoR match; all distinct netlists have complete bound LEC/STA'
            if unified else 'all selected cases included; exploratory early1-core Yosys attempts retained in raw data but not part of the selected matrix')})
    date=datetime.now(timezone.utc).date().isoformat()
    scope=f'''Five complete RISC-V designs × two PDKs. **One warm-up, then three measured runs**
for every selected configuration. Both tools receive **16 physical host cores** (CPU0–15,
no SMT siblings); RustSys uses `--threads16` and Yosys uses its default threading.
Each pair is **RustSys / Yosys**. RustSys0.1.0 and pinned Yosys0.67;
32 GiB RAM + 1 GiB swap per child, serial cases, AMD Ryzen9 9950X3D.
Prepared RTL through workspace init/import, synthesis, mmap and full Verilog export;
preparation, proof and STA are outside the timer. This is a shared host, not an isolated machine.
Peak RSS is the largest GNU-time process RSS across the three runs; aggregate
cgroup memory peaks, including compiler children, are retained in the raw receipts.
'''.replace('--threads16','--threads 16').replace('RustSys0.1.0','RustSys 0.1.0').replace('Yosys0.67','Yosys 0.67').replace('Ryzen9','Ryzen 9')
    if unified:
        scope='''Five complete RISC-V designs × two PDKs, with **both tools measured at 1 and 16 physical host cores**.
Each allocation uses one warm-up and three timed runs; the table shows medians.
RustSys uses area/medium mapping and its default area-capped quality cleanup;
Yosys 0.67 uses the same arbitrary-initial-state-preserving/default-ABC flow for every design.
RustSys worker count follows the CPU allowance. Both tools use CPU0 at one core and CPU0–15 at 16 cores,
with no SMT siblings. Host: AMD Ryzen 9 9950X3D; child limits:32GiB RAM+1GiB swap.

Time covers prepared RTL through workspace init/import, synthesis, mmap and full Verilog export.
Preparation, LEC and OpenSTA are outside the timer. Timed cases run serially on a shared host.
Average active cores = (user+system CPU seconds)/wall seconds, median across three runs.
Each tool's 1-core and16-core netlists are byte-identical, so each displayed area/Fmax/LEC result applies to both.
Peak process RSS and aggregate cgroup peaks remain in the complete measurement receipts.
New Yosys1-core measurements and unchanged earlier Yosys16-core samples retain their separate provenance.
'''.replace('CPU0','CPU 0').replace('limits:32GiB RAM+1GiB','limits: 32 GiB RAM + 1 GiB').replace('and16-core','and 16-core').replace('Yosys1-core','Yosys 1-core').replace('Yosys16-core','Yosys 16-core')
    electrical_note=('The default cleanup is bounded and does not enforce every electrical constraint. '
        'These setup estimates do not establish achievable or signoff Fmax.' if unified else
        'Electrical repair is not performed; these raw estimates do not establish achievable or signoff Fmax.')
    electrical_path=a.validation/'openc910-electrical-scope.json'
    if electrical_path.exists():
        electrical=run.read_json(electrical_path)
        cases={r['tool']:r for r in electrical['rows']}
        recorded=next(r for r in timing['rows'] if r['id']=='openc910' and r['library']=='sky130hd_tt')
        if set(cases)!={'rustsys','yosys'}:raise ValueError('incomplete electrical-scope observation')
        for tool,case in cases.items():
            known={**recorded[tool]['sha256'],**recorded[tool]['receipts_sha256']}
            if any(known.get(p)!=h or run.digest(p)!=h for p,h in case['sha256'].items()):
                raise ValueError('electrical-scope observation does not bind the measured timing inputs')
        electrical_note=(
            ("OpenC910 exceeds the default cleanup budget. " if unified else "Electrical repair is not performed. ")+
            f"OpenC910/Sky130 critical-path loads exceed the Liberty characterization range "
            f"(RustSys/Yosys: {cases['rustsys']['load_pf']:.2f}/{cases['yosys']['load_pf']:.2f} pF; "
            f"respective cell limits {cases['rustsys']['cell_max_capacitance_pf']:.6f}/{cases['yosys']['cell_max_capacitance_pf']:.6f} pF). "
            'These raw estimates do not establish achievable or signoff Fmax.')
    timing_scope=f'''‡ OpenSTA3.1.0 setup-Fmax estimate at each library's TT corner: ideal clocks,
100 ps input/clock slew, 10 fF output load, zero I/O delays and no wire parasitics.
MHz = 1000 / reported minimum period in ns, within the report's printed precision.
Same-clock, same-edge register paths only; port paths, cross-edge paths and
level-sensitive latch paths are excluded by `report_clock_min_period`.
Unclocked or otherwise unconstrained endpoints are outside this estimate;
constraint-check receipts are retained. {electrical_note}'''.replace('OpenSTA3.1.0','OpenSTA 3.1.0')
    large='''### OpenC910 configuration and proof scope

The OpenC910 RTL top is `ct_core`: one complete processor core with default64KiB
instruction and64KiB data caches, lowered to standard cells. The16 cores above
are **host CPU resources**, not a change to the hardware configuration. L2 and
other processor cores are outside this RTL top. The source/caches are unchanged.

OpenC910 uses the same1warm-up/3sample protocol as every other design.
The common Yosys flow preserves arbitrary initial state and uses default ABC
library mapping for all five designs. RustSys uses area/medium mapping with
its4GiB cut-storage budget, retaining the effective cut counts in every report.

All netlists use the independent RTL event model with
ordinary X/Z data and undriven wires completed to zero, plus external ABC CEC.
Inferred multiply/add blocks use exact arithmetic components checked by Arisca,
with complete ABC checks of their Boolean context, component/context
reconstruction, and transport to the actual measured netlist.
The same semantic contract applies to every tool/design pair. Every result
keeps arbitrary implementation initial state under checked correspondence;
**no reset sequence or input assumption is added**.
'''
    large=large.replace('default64KiB','default 64 KiB').replace('and64KiB','and 64 KiB').replace('The16','The 16').replace('same1warm-up/3sample','same one-warm-up/three-sample').replace('bounded4-cut/1area-flow','bounded four-cut/one-area-flow').replace('its4GiB','its 4 GiB')
    readme=f'''# Common multicore synthesis benchmark, {date}

{scope}
{main}

{timing_scope}

{large}
## CPU scaling and quality preservation

{scale}

Average active cores = (GNU time user+system CPU seconds)/wall seconds,
median across three runs. This measures actual use, not requested workers or
peak utilization. RustSys 1→16-core scaling and RustSys/Yosys speedup are distinct.
Byte-identical 1-core and 16-core RustSys netlists preserve cell area and timing under the
same constraints. This does not claim universal PPA superiority over Yosys.

Speedup spans {min(speeds):.2f}×–{max(speeds):.2f}× across all ten measured pairs.
RustSys self-scaling spans {min(selfups):.2f}×–{max(selfups):.2f}×. No row is dropped.

All timing and proof records bind exact measured netlist/library bytes. Fresh
small-design proofs export every original output/state/event obligation and
require complete external CEC or arithmetic/context composition. Model export
alone is undecided. Exact-byte
full-core certificates can be reused only with their measured inputs unchanged.
Data and clock corruptions are rejected on both PDKs; signed/overflow arithmetic
calibration checks both low-bit and high-bit faults. Resource-limited/missing
results cannot be published as passes.
Frozen producer hashes identify the implementation measured in this campaign.
Source-campaign records identify separately collected reference measurements;
exact-byte proof or timing reuse is explicitly marked in each certificate and
does not represent a fresh solver or STA invocation.

[Measurements]({measurement_file}) · [LEC]({proof_file}) · [OpenSTA]({timing_file}) ·
[Controls]({control_file}) · [Raw receipts and harness]({evidence_file}) ·
[Large artifact hashes]({large_file}) · [Provenance](provenance.json).

Files above 1 MiB are split into ordered parts. Their index records the combined
SHA-256; concatenate the listed parts in order before decompressing. Raw data,
failed probes and larger models remain in the hash-listed local workspace.
Reproducible model payloads above the recorded copy limit are hash-listed;
small models and complete solver receipts are included in the streamed archive.
'''
    (out/'README.md').write_text(readme)
    (out/'SHA256SUMS').write_text(''.join(f'{run.digest(p)}  {p.name}\n' for p in sorted(out.iterdir()) if p.is_file() and p.name!='SHA256SUMS'))
    if a.root_readme:
        text=a.root_readme.read_text();before,after=text.split('## Benchmarks\n',1)
        _,rest=after.split(START,1)
        overview_scope=('Five complete RISC-V designs × two PDKs. **16 physical host cores per tool**, '
            'one warm-up and three measured runs; median wall times below.\n'
            'Time includes workspace init/import, synthesis, mmap and full Verilog export; '
            'source preparation, LEC and timing analysis are outside the timer.\n')
        text=before+'## Benchmarks\n\n'+overview_scope+'\n'+START+rest
        text=block(text,START,END,comparison_table(data,proofs,timing))
        end_index=text.index(END)+len(END);large_index=text.index(LARGE_START)
        summary=(f'Speedup = **Yosys time ÷ RustSys time**. All **20 distinct netlists pass '
            'arbitrary-state RTL LEC**, with no reset or input assumption added.\n\n'
            f'[Full 1/16-core results, CPU use, area, setup-Fmax and LEC certificates]({relative}/README.md).\n\n')
        text=text[:end_index]+'\n\n'+summary+text[large_index:]
        details='<details>\n<summary>Workload, proof and timing scope</summary>\n\n'+large+'\n'+timing_scope+'\n\n</details>'
        text=block(text,LARGE_START,LARGE_END,details)
        scale_section=('Both tools emit byte-identical netlists at 1 and 16 cores; '
            'thread count preserves measured area and timing. The complete campaign linked above '
            'retains the single-core comparisons and CPU scaling evidence.' if unified else
            ('### CPU scaling without changing the netlist\n\n'+scale+'\n\nAverage active cores = (user + system CPU seconds) / wall time, median of three runs.\nIdentical netlists preserve area and Fmax under the same constraints. The values show\nactual use of the allocated CPUs; they do not imply full16-core utilization.\n').replace('full16','full 16'))
        if SCALE_START in text:text=block(text,SCALE_START,SCALE_END,scale_section)
        else:text=text.replace('## Quick start',SCALE_START+'\n\n'+scale_section+'\n'+SCALE_END+'\n\n## Quick start',1)
        a.root_readme.write_text(text)
    print(main);print('Published complete campaign:',out)


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    for name in ('workspace','validation','controls','output'):p.add_argument('--'+name,type=Path,required=True)
    p.add_argument('--root-readme',type=Path)
    p.add_argument('--max-files',type=int,default=65536)
    p.add_argument('--max-model-bytes',type=int,default=32768)
    p.add_argument('--max-evidence-bytes',type=int,default=256*1024**2)
    publish(p.parse_args())
