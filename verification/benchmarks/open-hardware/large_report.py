#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Publish complete, bounded large-design evidence without treating synthesis as proof.

Planning: <=64 MiB of four-netlist reports/source snapshots, streamed SHA-256
over netlists, one process (<256 MiB RAM). No graph/model data is loaded. Run only
after the measurement process has finished; the destination must be fresh.
"""
from pathlib import Path
import argparse
import json
import os
import shutil

import report
import run
import large_lec
import slice_lec
import lec_counterexample

START = '<!-- large-hardware-benchmark:start -->'
END = '<!-- large-hardware-benchmark:end -->'
MAX_ARCHIVE_BYTES = 64 * 1024**2


def proven(proof):
    registry=run.HERE/'results/proof-invalidations.json'
    if registry.exists():
        sources=set(proof.get('source_sha256',{}).values())
        checker=proof.get('tools',{}).get('checker',{}).get('sha256')
        for notice in run.read_json(registry):
            if notice.get('prepared_sha256') in sources and checker in notice.get('checker_sha256',[]):
                return False
    return report.complete_external_proof(proof) or large_lec.complete(proof) or slice_lec.complete(proof)


def table(result, *, summary=False):
    rows = list(report.TABLE_HEADER) if summary else [
        '| PDK | Time, s ↓<br>RustSys / Yosys | Peak process RSS, MiB ↓<br>RustSys / Yosys | Standard cells<br>RustSys / Yosys | Cell area, µm² ↓<br>RustSys / Yosys | RTL LEC<br>RustSys / Yosys |',
        '|---|---:|---:|---:|---:|---|',
    ]
    for row in result['rows']:
        values = []
        fields = ('median_wall_s', 'peak_rss_kib', 'area_um2') if summary else ('median_wall_s', 'peak_rss_kib', 'cells', 'area_um2')
        for key in fields:
            pair = []
            for tool in ('rustsys', 'yosys'):
                data = row[tool]
                value = data.get('qor', {}).get(key) if key in ('cells', 'area_um2') else data.get(key)
                if data['status'] != 'ok' or value is None:
                    last=data.get('samples',[{}])[-1]
                    pair.append('OOM' if key=='median_wall_s' and last.get('status')=='memory_limit' else '—')
                elif key == 'median_wall_s':
                    pair.append(f'{value:,.2f}')
                elif key == 'peak_rss_kib':
                    pair.append(f'{value/1024:,.0f}')
                else:
                    pair.append(f'{value:,.0f}')
            values.append(' / '.join(pair))
        proofs = [('Proven†' if row[t]['equivalence'].get('method') in (large_lec.METHOD,slice_lec.METHOD) else 'Proven')
                  if proven(row[t]['equivalence']) else ('Not equivalent†' if lec_counterexample.complete(row[t]['equivalence']) else 'Unverified')
                  for t in ('rustsys', 'yosys')]
        name = {'sky130hd_tt': 'Sky130 HD', 'gf180mcu7t_tt': 'GF180 7T'}[row['library']]
        core = [row['design'] + '†'] if summary else []
        rows.append('| ' + ' | '.join([*core, name, *values, ' / '.join(proofs)]) + ' |')
    return '\n'.join(rows)


def publish(args):
    measurements = args.measurements.resolve()
    result = run.read_json(measurements / 'results.json')
    if args.baseline_proofs:
        baseline=run.read_json(args.baseline_proofs/'combined-results.json')
        if (baseline['tools']!=result['tools'] or baseline['limits']!=result['limits']
                or len(baseline['rows'])!=len(result['rows'])):
            raise ValueError('baseline proof dataset differs from measured profile')
        for row,other in zip(result['rows'],baseline['rows'],strict=True):
            if row['library']!=other['library'] or row['yosys']['samples']!=other['yosys']['samples']:
                raise ValueError('baseline proof changed measured samples')
            row['yosys']['equivalence']=other['yosys']['equivalence']
    if args.proofs:
        combined=run.read_json(args.proofs/'combined-results.json')
        def numeric(data):
            import copy
            out=copy.deepcopy(data)
            for row in out['rows']:
                for tool in ('rustsys','yosys'):row[tool].pop('equivalence',None)
            return out
        if numeric(result)!=numeric(combined):
            # A proof may follow an exactly byte-identical measured netlist to
            # a new workspace/resource profile. Never transfer by design name.
            for row in result['rows']:
                old=next((r for r in combined['rows'] if r['library']==row['library']),None)
                if old is None:raise ValueError('proof dataset is missing a PDK')
                for tool in ('rustsys','yosys'):
                    proof=old[tool].get('equivalence',{})
                    if not proven(proof):continue
                    if row[tool]['status']!='ok':raise ValueError('cannot attach a proof to failed synthesis')
                    first=row[tool]['samples'][0];prior=old[tool]['samples'][0]
                    if first['netlist_sha256']!=prior['netlist_sha256'] or row['library_sha256']!=old['library_sha256']:
                        raise ValueError('proof transport requires identical netlist and PDK content')
                    original=Path(next(p for p,h in proof['source_sha256'].items() if h==prior['netlist_sha256']))
                    if run.digest(original)!=prior['netlist_sha256']:raise ValueError('original proof netlist changed')
                    target=measurements/row['library']/(tool+'-1')/'netlist.v'
                    if run.digest(target)!=prior['netlist_sha256']:raise ValueError('new measured netlist differs')
                    import copy
                    transported=copy.deepcopy(proof);transported['source_sha256'][str(target)]=prior['netlist_sha256']
                    transported['artifact_transport']={'original_netlist':str(original),'new_netlist':str(target),
                        'netlist_sha256':prior['netlist_sha256'],'byte_identity_checked':True,
                        'original_result_sha256':run.digest(args.proofs/'combined-results.json')}
                    row[tool]['equivalence']=transported
        else:result=combined
    expected = run.read_json(run.HERE / 'large-designs.json')
    if len(result['rows']) != 2 or {r['library'] for r in result['rows']} != set(expected['libraries']):
        raise ValueError('both pinned PDK measurements are required')
    if result['design']['sha256'] != {'openc910.v': expected['prepared_sha256']}:
        raise ValueError('complete input core checksum differs')
    artifact_hashes = []
    for row in result['rows']:
        for tool in ('rustsys', 'yosys'):
            data = row.get(tool)
            if data is None or len(data.get('samples', [])) != result['limits']['repetitions']:
                raise ValueError('measurement is incomplete')
            if data['status'] == 'incomplete':
                raise ValueError('measurement is still running')
            for rep, sample in enumerate(data['samples'], 1):
                path = measurements / row['library'] / f'{tool}-{rep}' / 'netlist.v'
                if sample['status'] == 'ok':
                    if run.digest(path) != sample['netlist_sha256']:
                        raise ValueError('measured netlist has changed')
                    artifact_hashes.append({'path': str(path), 'bytes': path.stat().st_size,
                                            'sha256': sample['netlist_sha256']})
            proof = data['equivalence']
            if proof.get('verdict') == 'proven' and not proven(proof):
                raise ValueError('proof claim lacks a complete external certificate')
            if proof.get('verdict') == 'not-equivalent' and not lec_counterexample.complete(proof):
                raise ValueError('inequivalence claim lacks a replayed external output counterexample')
            if proof.get('verdict') == 'proven':
                first_path=measurements/row['library']/(tool+'-1')/'netlist.v'
                if proof.get('source_sha256',{}).get(str(first_path))!=data['samples'][0]['netlist_sha256']:
                    raise ValueError('proof is not attached to this exact measured netlist')
    if (measurements / 'failure.json').exists():
        raise ValueError('measurement failed its final provenance checks')
    telemetry=run.read_json(measurements/'cgroup-telemetry.json') if (measurements/'cgroup-telemetry.json').exists() else {}
    failures=[]
    for row in result['rows']:
        for tool in ('rustsys','yosys'):
            for rep,sample in enumerate(row[tool]['samples'],1):
                if sample['status']=='ok':continue
                sample['collector_peak_rss_kib']=sample.pop('peak_rss_kib',None)
                sample['peak_rss_kib']=None
                sample['peak_rss_status']='unavailable; failed-scope timer measured the reporter, not the compiler'
                observed=telemetry.get(row['library']+'/'+tool+'-'+str(rep),{})
                sample['cgroup_telemetry']=observed
                if observed.get('last_scope_result')=='oom-kill':sample['status']='memory_limit'
                failures.append(f"{row['library']} / {tool}: {sample['status']} after {sample['wall_s']:.2f} seconds")
    baseline_note=('Yosys LEC remains unverified because netlist import exceeded the 8 GiB verification guard.'
        if any(r['yosys']['equivalence'].get('diagnostic',{}).get('code')=='BENCH_LEC_IMPORT_MEMORY_LIMIT' for r in result['rows']) else '')
    if any(lec_counterexample.complete(r['yosys']['equivalence']) for r in result['rows']):
        baseline_note='The measured Yosys profile fails arbitrary-initial-state equivalence: its merged lpmd_b bits exclude the RTL initial values 01 and 10. An external ABC output counterexample is replayed in both original AIGER models; no reset assumption is added.'
    result['host'] = {
        'cpu': next(s.split(':', 1)[1].strip() for s in Path('/proc/cpuinfo').read_text().splitlines()
                    if s.startswith('model name')),
        'kernel': os.uname().release,
        'metadata_collected': 'at publication; measurement CPU affinity recorded separately',
    }
    destination = args.destination.resolve()
    destination.mkdir(exist_ok=False)
    total = 0
    entries = {}

    def copy(source, relative):
        nonlocal total
        size = source.stat().st_size
        if source.suffix in ('.stdout','.stderr') and size>4*1024**2:
            artifact_hashes.append({'path':str(source),'bytes':size,'sha256':run.digest(source),
                                    'kind':'large raw tool log; retained in benchmark workspace'})
            return
        total += size
        if total > MAX_ARCHIVE_BYTES:
            raise ValueError('evidence archive byte budget reached')
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        entries[str(relative)] = run.digest(target)

    for row in result['rows']:
        for tool in ('rustsys', 'yosys'):
            for rep, _ in enumerate(row[tool]['samples'], 1):
                folder = measurements / row['library'] / f'{tool}-{rep}'
                for name in ('config.json', 'pipeline.json', 'synth.ys', 'synth.command.json',
                             'synth.status.json', 'synth.time', 'synth.stdout', 'synth.stderr', 'report.json'):
                    source = folder / name
                    if source.exists(): copy(source, Path(row['library']) / f'{tool}-{rep}' / name)
    for name in ('input.json', 'preprocess.status.json', 'preprocess.time', 'preprocess.command.json'):
        source = args.prepared / name
        if source.exists(): copy(source, Path('preparation') / name)
    for name in ('producer-sources.tar.gz', 'producer-sources.json', 'checker-sources.tar.gz',
                 'checker-sources.json', 'slicer-sources.tar.gz', 'slicer-sources.json',
                 'slicer-runner-update.json', 'verification-runtime-summary.json',
                 'reference-executables.json', 'baseline-failure.json',
                 'checker-lec-finish-sources.tar.gz','checker-lec-finish-sources.json',
                 'producer-wildcard-fixed-sources.tar.gz','producer-wildcard-fixed-sources.json',
                 'checker-wildcard-fixed-sources.tar.gz','checker-wildcard-fixed-sources.json'):
        source = args.evidence / name
        if source.exists(): copy(source, Path('provenance') / name)
    telemetry = measurements / 'cgroup-telemetry.json'
    if telemetry.exists(): copy(telemetry, Path('provenance/cgroup-telemetry.json'))
    for source in sorted(run.HERE.glob('*.py')):
        copy(source, Path('harness') / source.name)
    invalidations=run.HERE/'results/proof-invalidations.json'
    if invalidations.exists():copy(invalidations,Path('provenance/proof-invalidations.json'))
    for name in ('designs.json','large-designs.json', 'latches-sky130.v', 'latches-gf180.v'):
        copy((run.LEC_FIXTURES if name.endswith('.v') else run.HERE) / name, Path('harness') / name)
    for control in ('frontend-controls-final', 'bounded-mux-controls-final', 'checker-yosys-controls3',
                    'frozen-yosys-controls','aiger-export-controls2','aiger-truth-controls',
                    'slice-controls','slice-wide-controls','stream-import-controls2',
                    'free-state-export-controls2','initial-preserving-controls-final2',
                    'initial-preserving-fast-controls','initial-preserving-bounded-controls',
                    'parallel-slice-controls','parallel-slice-regression',
                    'wildcards-before2','wildcards-after','wildcard-checker-controls'):
        source = args.evidence / control / 'validation.json'
        if source.exists(): copy(source, Path('controls') / (control + '.json'))
    for name in ('check_frontend_fixes_final.py','check_mux_fix_final.py','check_yosys_frontend.py',
                 'check_frozen_yosys.py','check_aiger_export.py','check_aiger_truth.py',
                 'check_slices.py','check_slices_wide.py','smoke_final_cli.py',
                 'check_stream_import.py','check_free_state_export.py','check_initial_preserving_final2.py',
                 'check_initial_preserving_fast.py','measure_initial_yosys2.py','measure_initial_yosys_fast.py',
                 'check_initial_preserving_bounded.py','check_parallel_slices.py','check_parallel_regression.py',
                 'measure_initial_yosys_bounded.py','finish_all_raw_netlists.py','finish_yosys_lec_projected.py',
                 'check_wildcards_external.py','check_wildcard_checker.py','measure_rustsys_wildcard_fixed.py',
                 'export_yosys_fixed_oracle.py','export_rustsys_fixed_oracle.py'):
        source=args.evidence/name
        if source.exists():copy(source,Path('controls/harness')/name)
    for name in ('legacy_params_comments.v','broad_mux_control.v','module_loop_counters.v','event_cec.v','aiger_ops.v','wildcard_structures.v'):
        source=run.ROOT/'verification/fixtures/synthetic/synth/rtl'/name
        copy(source,Path('controls/rtl')/name)
    copy(run.ROOT/'verification/fixtures/synthetic/synth/wildcard-icarus-truth.json',Path('controls/rtl/wildcard-icarus-truth.json'))
    if args.proofs:
        for source in sorted(args.proofs.glob('*/*/*')):
            if source.is_file() and source.suffix in ('.json','.stdout','.stderr','.time'):
                copy(source,Path('lec')/source.relative_to(args.proofs))
        for source in sorted(args.proofs.glob('*/*/obligations/manifest.json')):
            copy(source,Path('lec')/source.relative_to(args.proofs))
            manifest=run.read_json(source)
            for side in manifest['sides']:
                large=source.parent/side['file']
                artifact_hashes.append({'path':str(large),'bytes':large.stat().st_size,'sha256':run.digest(large)})
            for name in ('state-relation.jsonl','input-variables.u32le'):
                large=source.parent/name
                artifact_hashes.append({'path':str(large),'bytes':large.stat().st_size,'sha256':run.digest(large)})
        for source in sorted(args.proofs.glob('*/*/check*/certificate.json')):
            copy(source,Path('lec')/source.relative_to(args.proofs))
        for row in result['rows']:
            for tool in ('rustsys','yosys'):
                proof=row[tool]['equivalence']
                if proof.get('method')!=slice_lec.METHOD:continue
                comparison=proof['certificate']['comparison']
                native=Path(next(p for p in comparison['source_sha256'] if Path(p).name=='gold.aig')).parent
                copy(native/'manifest.json',Path('lec')/row['library']/tool/'source-model/manifest.json')
                for source in sorted(native.parent.iterdir()):
                    if source.is_file() and source.suffix in ('.json','.stdout','.stderr','.time'):
                        copy(source,Path('lec')/row['library']/tool/'source-model'/source.name)
                for name in ('gold.aig','gate.aig','state-relation.jsonl','input-variables.u32le'):
                    large=native/name;artifact_hashes.append({'path':str(large),'bytes':large.stat().st_size,'sha256':run.digest(large)})
                import shlex
                checks=args.proofs/row['library']/tool
                first=next(checks.glob('check*/abc.command.json'))
                command=run.read_json(first)[-1]
                projected=Path(shlex.split(command.rstrip(';'))[-2]).parent.parent
                for item in comparison['slices']:
                    directory=projected/f"slice{item['id']:05}"
                    for name,sha in item['sha256'].items():
                        large=directory/name
                        if run.digest(large)!=sha:raise ValueError('verified projection artifact changed')
                        artifact_hashes.append({'path':str(large),'bytes':large.stat().st_size,'sha256':sha})
    if args.baseline_proofs:
        for source in sorted(args.baseline_proofs.glob('*/*/*')):
            if source.is_file() and source.suffix in ('.json','.stdout','.stderr','.time'):
                copy(source,Path('baseline-lec')/source.relative_to(args.baseline_proofs))
        for row in result['rows']:
            proof=row['yosys']['equivalence']
            if not lec_counterexample.complete(proof):continue
            for name,path in proof['certificate']['original_aiger_files'].items():
                large=Path(path)
                artifact_hashes.append({'path':str(large),'bytes':large.stat().st_size,'sha256':run.digest(large)})
            native=Path(proof['certificate']['original_aiger_files']['gold']).parent
            for name in ('state-relation.jsonl','input-variables.u32le'):
                large=native/name
                artifact_hashes.append({'path':str(large),'bytes':large.stat().st_size,'sha256':run.digest(large)})
    run.save(destination / 'results.json', result)
    run.save(destination / 'large-artifacts.json', artifact_hashes)
    text = table(result)
    flow = 'Yosys uses the additional public `synth -noshare` option.' if not result['scope']['yosys_resource_sharing'] else 'Yosys resource sharing is enabled.'
    if result['scope'].get('yosys_flow') in ('arbitrary-initial-state-v1','arbitrary-initial-state-fast-v1'):
        flow='Yosys uses the separately labelled arbitrary-initial-state flow in `yosys_initial.py`: structural memory lowering, explicit `opt_merge -keepdc` and `opt_dff -keepdc`, and no `wreduce` state elimination. Literal X/Z completion does not set register initial values (`setundef -init` is never used).'
        if result['scope']['yosys_flow'].endswith('-fast-v1'):
            flow+=' Mapping uses the public `abc -fast` option; its runtime and QoR belong to this fast-mapping profile.'
    flow_brief=flow
    if result['scope'].get('yosys_flow')=='arbitrary-initial-state-fast-v1':
        flow_brief='Yosys uses a separate arbitrary-initial-state profile with `abc -fast` mapping.'
    if result['scope'].get('yosys_flow')=='arbitrary-initial-state-bounded-map-v1':
        flow='Yosys uses the arbitrary-initial-state flow in `yosys_initial.py`, structural memory lowering, `opt_merge -keepdc` and `opt_dff -keepdc`. Mapping uses the public ABC `strash; &get -n; &nf -C 4 -F 1; &put` script: at most four priority cuts and one area-flow round. No register initial value or reset assumption is introduced.'
        flow_brief='Yosys uses a separate arbitrary-initial-state profile with four mapping cuts and one area-flow round.'
    rust_memory=result['scope'].get('rustsys_memory_gib',result['limits']['physical_memory_gib'])
    yosys_memory=result['scope'].get('yosys_memory_gib',result['limits']['physical_memory_gib'])
    memory=(f'{rust_memory} GiB physical RAM per tool' if rust_memory==yosys_memory
            else f'{rust_memory} GiB physical RAM for RustSys / {yosys_memory} GiB for Yosys')
    reuse=('RustSys samples are reused unchanged from the earlier measured run, with source, library, producer and netlist hashes retained. Their original resource guard is stated above.'
           if result['scope'].get('rustsys_sample_reuse') else '')
    if result['scope'].get('yosys_sample_reuse'):
        reuse='Yosys samples and netlist bytes are retained unchanged from the bounded-mapping reference run. RustSys was freshly measured after the wildcard correction; both tools use the 32 GiB guard. Proof transport requires identical RTL, library and netlist hashes.'
    readme = f'''# OpenC910 large-design stress measurement

Pinned [OpenC910]({result['design']['repo']}/tree/{result['design']['commit']}), Apache-2.0,
top `ct_core`: **complete single core**, default 64 KiB instruction and 64 KiB data caches.
Every upstream functional FPGA RAM model is lowered to standard cells inside the timer.
L2, the second core and cluster peripherals are outside this top. No SRAM black boxes.

{text}

Recorded synthesis failures: {'; '.join(failures) if failures else 'none'}.

One measured run per tool and library, no warm-up; this is a stress measurement, not
the three-run median of the four-core table. One pinned logical CPU ({result['limits']['cpu_affinity'][0]}),
{memory}, plus 1 GiB swap per child,
{result['limits']['wall_s']} seconds per tool. Time includes init/import, synthesis,
mmap and full Verilog export. Preparation and proof attempts are outside the timer.
No repeat-determinism claim is made for a single sample.
{reuse}

{flow} The earlier default-profile probe failed at its 16 GiB child
memory limit; its failure is preserved in `provenance/baseline-failure.json`.
RustSys uses area/medium mapping with a 4 GiB cut-storage budget; consult each
`report.json` for requested and effective priority cuts. Resource-limited cut selection
changes the area/search tradeoff; it never shrinks the RTL or caches.

`Proven` requires a complete external certificate against this exact measured netlist.
`Proven†` uses the independent RTL interpreter with ordinary X/Z data literals set to zero,
undriven wires set to zero, and external ABC CEC of every output, state update,
clock/enable and asynchronous clear/preset obligation. Exact shared-structure
identities are discharged directly. Each remaining output cone is projected
without changing its Boolean function, with one common input map for both sides;
the interval ledger must cover every original obligation once. Uninitialized
registers remain arbitrary. Wildcard labels retain their `casez`/`casex` matching
semantics through concatenation and replication. Its named state relation covers every implementation
initial state. The native export itself is recorded as undecided; only ABC can
complete this certificate. The four-core table has its separately stated proof scope.
An em dash means the tool did not complete that metric. `Unverified` is not a pass.
Successful RSS is GNU time's maximum process RSS, including waited-for children;
cgroup aggregate memory/swap peaks are recorded separately. Failed-scope reporter
RSS is retained only as collector telemetry and is unavailable as compiler RSS.
All proof attempts retain arbitrary initial states and add no reset or input assumptions.
{baseline_note}
Corner is per library; wire model: none; all endpoints unconstrained; STA and DRC unchecked.

`results.json` retains raw samples, corners, PDK hashes and executable pins.
`large-artifacts.json` hashes the large generated files kept in the local benchmark
workspace; those multi-gigabyte artifacts are not copied into this bounded archive.
Raw tool logs above 4 MiB also remain in that hash inventory; commands, exit
statuses, timing reports and complete QoR JSON remain in the archive.
The producer source snapshot and exact build flags are in `provenance/`.
This corrected run uses `producer-wildcard-fixed-sources.json` and
`checker-wildcard-fixed-sources.json`. Earlier OpenC910 native certificates are
invalidated in `provenance/proof-invalidations.json`. The independent Icarus
truth table, exhaustive before/after results, Yosys formal comparisons and
negative old-netlist controls are in `controls/`; both complete Yosys baseline
proofs calibrated the corrected RTL interpreter before accepting RustSys.
Reproduction is documented in the [benchmark guide](../../README.md#large-design-stress-run).
'''
    (destination / 'README.md').write_text(readme)
    for name in ('results.json', 'large-artifacts.json', 'README.md'):
        entries[name] = run.digest(destination / name)
    run.save(destination / 'checksums.json', entries)
    root = args.readme.resolve()
    relative = destination.relative_to(run.ROOT).as_posix()
    cell_counts = [row['rustsys'].get('qor', {}).get('cells') for row in result['rows']]
    scale_note = 'Exact cell counts for both tools are in the linked evidence.'
    if (cell_counts and all(type(count) is int and count > 0 for count in cell_counts)
            and all(row['rustsys']['status'] == 'ok' for row in result['rows'])):
        scale_note = (f'RustSys maps this design to approximately **{min(cell_counts)/1e6:.1f}–{max(cell_counts)/1e6:.1f} million standard cells**;\n'
                      'exact counts for both tools are in the linked evidence.')
    section = f'''{START}

### OpenC910: full single-core stress test

OpenC910 includes 64 KiB instruction and 64 KiB data caches, fully lowered to
standard cells. L2 and the second core are outside this top. One measured run
per tool/PDK, one pinned CPU, {memory}, plus 1 GiB swap per child,
and a {result['limits']['wall_s']}-second deadline per tool. The `OpenC910†` rows in the
main table use this single-run stress profile; the other cores use three-run medians.
{scale_note}

{flow_brief} RustSys limits cut storage to 4 GiB (40 requested / 7 effective cuts).
† Independent RTL model with ordinary X/Z data literals and undriven wires set to zero, plus
ABC CEC; register initial states remain arbitrary under checked correspondence,
with no reset or input assumption. {baseline_note}
[Scope, limits and evidence]({relative}/README.md).

{END}'''
    old = root.read_text()
    if START in old:
        before, tail = old.split(START, 1)
        _, after = tail.split(END, 1)
        old = before + section + after
    else:
        anchor = '## Quick start' if '## Quick start' in old else '## Highlights'
        if anchor not in old:
            raise ValueError('README is missing an insertion point for the stress-profile notes')
        old = old.replace(anchor, section + '\n\n' + anchor, 1)
    root.write_text(report.update_readme_table(old, table(result, summary=True)))
    print(text)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    for name in ('measurements', 'prepared', 'evidence', 'destination'):
        p.add_argument('--'+name, type=Path, required=True)
    p.add_argument('--proofs',type=Path)
    p.add_argument('--baseline-proofs',type=Path)
    p.add_argument('--readme',type=Path,default=run.ROOT/'README.md')
    publish(p.parse_args())
