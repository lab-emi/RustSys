#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Publish a bounded, complete five-design runtime audit with exact-byte LEC.

<=12 cohorts/4096 samples, <=64 MiB source JSON, <1 GiB planned RSS.
No graph or solver is constructed. Hash files in 1 MiB chunks; archive <=131072
files/256 MiB copied receipts. Run serially under a 2 GiB/1800 s OS guard,
after all timed jobs. Existing proof/timing reuse is never a fresh solver run.
"""
from pathlib import Path
import argparse
import gzip
import os
import tarfile

import evidence_reuse
import functional_models
import observed_proof
import paired_statistics
import refresh_report
import run
import scaling_report
import scaling_verify


class CheckedFiles:
    def __init__(self):
        self.checked = {}

    def pins(self, pins):
        for name, digest in pins.items():
            path = Path(name.replace('$RUSTSYS_ROOT', str(run.ROOT)))
            key = str(path.resolve())
            if key in self.checked:
                if self.checked[key] != digest: raise ValueError('conflicting file pin: ' + key)
            else:
                if len(self.checked) >= 131072: raise ValueError('checked-file budget')
                if run.digest(path) != digest: raise ValueError('changed evidence: ' + str(path))
                self.checked[key] = digest


def validate_receipts(validation, checked):
    if sum((validation / name).stat().st_size for name in
           ('complete-all.json', 'measurements.json', 'proofs.json', 'timing.json', 'quality.json')) > 64 * 1024**2:
        raise ValueError('prior-validation JSON byte budget')
    completion = run.read_json(validation / 'complete-all.json')
    if completion.get('status') != 'complete': raise ValueError('incomplete prior validation')
    checked.pins({str(validation / n): h for n, h in completion['artifacts'].items()})
    checked.pins(completion['sources'])
    data, proofs, timing = [run.read_json(validation / (n + '.json'))
                            for n in ('measurements', 'proofs', 'timing')]
    comparison = refresh_report.comparison(data, proofs, timing)
    checked.pins({**data['input_sha256'], **data['binary_sha256'], **data['source_campaigns']})
    rows = {(r['id'], r['library']): r for r in data['rows']}
    result = []
    for row in proofs['rows']:
        key = row['id'], row['library']
        sta_row = next(r for r in timing['rows'] if (r['id'], r['library']) == key)
        for tool in ('rustsys', 'yosys'):
            proof = row[tool]; sta = sta_row[tool]
            checked.pins(proof['source_sha256'])
            checked.pins(proof.get('artifact_sha256', {}))
            checked.pins(proof.get('receipts_sha256', {}))
            checked.pins({**sta['sha256'], **sta['receipts_sha256']})
            for record in proof.get('tools', {}).values():
                if isinstance(record, dict) and 'path' in record and 'sha256' in record:
                    checked.pins({record['path']: record['sha256']})
            proof_comparison = proof.get('certificate', {}).get('comparison', {})
            checked.pins(proof_comparison.get('source_sha256', {}))
            checked.pins(proof_comparison.get('tool_sha256', {}))
            if proof.get('method') == observed_proof.METHOD: observed_proof.validate(proof)
            supplement = proof.get('functional_library_supplement')
            if supplement:
                checked.pins({supplement['certificate']: supplement['certificate_sha256']})
                functional_models.validate(run.read_json(Path(supplement['certificate'])))
            if not scaling_verify.complete(proof): raise ValueError('incomplete arbitrary-state proof')
            result.append({'id': key[0], 'library': key[1], 'tool': tool,
                           'netlist_sha256': rows[key][tool]['netlist_sha256'],
                           'area_um2': rows[key][tool]['qor']['area_um2'], 'fmax_mhz': sta['fmax_mhz'],
                           'verdict': 'proven', 'fresh_solver_run': False,
                           'method': proof['method'], 'scope': proof['scope'],
                           'timing_scope': sta['scope'], 'fresh_sta_run': False})
            print('Revalidated LEC/STA:', *key, tool, flush=True)
    quality = run.read_json(validation / 'quality.json')
    if len(quality['rows']) != 15 or any(not r['area_nonregressing'] or not r['fmax_nonregressing']
                                        for r in quality['rows']):
        raise ValueError('prior quality regression')
    return rows, proofs, result, comparison


def validate_cohort(path, reference, proofs, checked):
    marker = run.read_json(path / 'complete.json')
    if marker.get('status') != 'complete': raise ValueError('incomplete cohort')
    checked.pins({str(path / 'results.json'): marker['results_sha256']})
    data = run.read_json(path / 'results.json')
    checked.pins(data['identity']['source_sha256']); checked.pins(data['identity']['tools_sha256'])
    checked.pins({str(path / 'harness' / name): h for name, h in data['harness_sha256'].items()})
    count = 0
    for block in data['blocks']:
        key = block['id'], block['library']
        if key not in reference: raise ValueError('unvalidated design/PDK')
        if data['identity']['cpu_affinity'] != reference[key]['cpu_affinity']:
            raise ValueError('audit CPU allocation differs from quality reference')
        row = reference[key]['rustsys']
        proof = next(r['rustsys'] for r in proofs['rows'] if (r['id'], r['library']) == key)
        for entry in block['samples']:
            count += 1
            if count > 1024: raise ValueError('cohort sample budget')
            sample = entry['sample']; cfg = run.read_json(Path(entry['config']))
            folder = Path(cfg['directory']); digest = row['netlist_sha256']
            if (sample['status'] != 'ok' or sample['netlist_sha256'] != digest
                    or sample['qor'] != row['qor'] or cfg['threads'] != 16 or cfg['tool'] != 'rustsys'
                    or entry['netlist'] != str(folder / 'netlist.v')):
                raise ValueError('changed or incomplete measured implementation')
            version = 'candidate' if block['kind'] == 'same_binary' else entry['label']
            bins = Path(data['identity']['variants'][version])
            if entry['version'] != version or any(cfg[n] != str(bins / n) for n in ('rustsys', 'rusteda')):
                raise ValueError('producer differs from schedule')
            checked.pins({entry['netlist']: digest, str(folder / 'clock.json'): entry['clock_sha256']})
            clock = run.read_json(folder / 'clock.json')
            if clock['exit_code'] != 0 or clock['wall_ns'] / 1e9 != sample['precise_wall_s']:
                raise ValueError('native timer binding differs')
            command = run.read_json(folder / 'synth.command.json')
            if (command[command.index('-c') + 1] != ','.join(map(str, data['identity']['cpu_affinity']))
                    or command[-3:] != [str(path / 'harness/run.py'), '_pipeline', entry['config']]):
                raise ValueError('timed pipeline/CPU command differs')
            pins = proof['source_sha256']
            inputs = {str(Path(p).resolve()): run.digest(p) for p in [cfg['library'], *cfg['design']['files']]}
            if any(pins.get(p) != h for p, h in inputs.items()): raise ValueError('proof RTL/library differs')
            if not evidence_reuse.same_top(proof, pins, digest, cfg): raise ValueError('proof top differs')
    family = 15 if any(b['library'] != 'gf180mcu7t_tt' for b in data['blocks']) else 5
    return data, paired_statistics.analyze(data, family), count


def table(statistics):
    lines = ['| Design · GF180 7T | A/B pairs | Before, s | Current, s | Paired median Δ, ms | 99% median interval, ms |',
             '|---|---:|---:|---:|---:|---:|']
    for did in scaling_report.ORDER:
        r = next(r for r in statistics['rows'] if r['id'] == did); s = r['comparison']
        ci = s['paired_median_delta_99pct_exact_ci_s']
        if ci is None: raise ValueError('missing primary median interval')
        name = {'ibex': 'Ibex', 'picorv32': 'PicoRV32', 'serv': 'SERV',
                'vexriscv-verilog': 'VexRiscv', 'openc910': 'OpenC910'}[did]
        lines.append(f"| {name} | {s['pairs']} | {s['baseline_median_s']:.4f} | {s['candidate_median_s']:.4f} | "
                     f"{1000*s['paired_median_delta_s']:+.3f} | [{1000*ci[0]:+.3f}, {1000*ci[1]:+.3f}] |")
    return '\n'.join(lines)


def controls_table(statistics):
    lines = ['| Design · GF180 7T | A/A pairs | Same-binary median Δ, ms | 99% median interval, ms |',
             '|---|---:|---:|---:|']
    for did in scaling_report.ORDER:
        row = next(r for r in statistics['rows'] if r['id'] == did)
        s = row['same_binary_control']; ci = s['paired_median_delta_99pct_exact_ci_s']
        text = f'[{1000*ci[0]:+.3f}, {1000*ci[1]:+.3f}]' if ci else 'Not estimable with two pairs'
        name = {'ibex': 'Ibex', 'picorv32': 'PicoRV32', 'serv': 'SERV',
                'vexriscv-verilog': 'VexRiscv', 'openc910': 'OpenC910'}[did]
        lines.append(f"| {name} | {s['pairs']} | {1000*s['paired_median_delta_s']:+.3f} | {text} |")
    return '\n'.join(lines)


def other_pdk_table(reports):
    record = next((r for r in reports if r['cohort'] == 'other-pdk-paired'), None)
    if record is None: return ''
    lines = ['## Sky130/EZ130 paired controls', '',
             'Separate 9afbf2b versus repair comparisons: 20 pairs per smaller design/PDK,',
             'two excluded paired warm-ups and four same-binary controls; Holm family size 15.',
             'All selected netlists, cell inventories, areas and raw setup estimates are unchanged.', '',
             '| Design | PDK | A/B pairs | Paired median Δ, ms | 99% median interval, ms |',
             '|---|---|---:|---:|---:|']
    for r in record['statistics']['rows']:
        s = r['comparison']; ci = s['paired_median_delta_99pct_exact_ci_s']
        label = {'ibex': 'Ibex', 'picorv32': 'PicoRV32', 'serv': 'SERV', 'vexriscv-verilog': 'VexRiscv'}[r['id']]
        lines.append(f"| {label} | {refresh_report.LABELS[r['library']]} | {s['pairs']} | "
                     f"{1000*s['paired_median_delta_s']:+.3f} | [{1000*ci[0]:+.3f}, {1000*ci[1]:+.3f}] |")
    return '\n'.join(lines)


def archive(output, roots):
    copied = []; omitted = []; total = count = 0
    suffixes = {'.json', '.py', '.rs', '.sh', '.time', '.stdout', '.stderr', '.log', '.md'}
    for prefix, root in roots:
        for path in root.rglob('*'):
            if path.is_symlink(): raise ValueError('archive symlink')
            if not path.is_file() or '__pycache__' in path.parts: continue
            if path.name in {'progress.json', 'experimental-repair.patch'}: continue
            count += 1
            if count > 131072: raise ValueError('archive file budget')
            size = path.stat().st_size; name = str(Path(prefix) / path.relative_to(root))
            if path.suffix not in suffixes or size > 1024**2:
                omitted.append({'path': str(path), 'archive_path': name, 'bytes': size, 'sha256': run.digest(path)})
            else:
                total += size
                if total > 256 * 1024**2: raise ValueError('archive receipt-byte budget')
                copied.append((path, name))
    with (output / 'evidence.tar.gz').open('wb') as stream:
        with gzip.GzipFile(fileobj=stream, mode='wb', filename='', mtime=0) as zipped:
            with tarfile.open(fileobj=zipped, mode='w|') as tar:
                for path, name in sorted(copied, key=lambda item: item[1]):
                    info = tar.gettarinfo(str(path), arcname=name)
                    info.uid = info.gid = 0; info.uname = info.gname = ''; info.mtime = 0
                    with path.open('rb') as content: tar.addfile(info, content)
    return {'receipts': scaling_report.split_large(output / 'evidence.tar.gz'),
            'inventory': scaling_report.packed_json(output / 'large-artifacts.json.gz', omitted),
            'receipt_files': len(copied), 'receipt_bytes': total, 'omitted_files': len(omitted)}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ('audit-root', 'validation', 'output'): p.add_argument('--' + name, type=Path, required=True)
    p.add_argument('--primary', type=Path, action='append', required=True)
    p.add_argument('--supplement', type=Path, action='append', default=[])
    a = p.parse_args()
    a.primary = [path.resolve() for path in a.primary]
    a.supplement = [path.resolve() for path in a.supplement]
    cohorts = a.primary + a.supplement
    if not 1 <= len(a.primary) <= 5 or len(cohorts) > 12 or len(set(cohorts)) != len(cohorts):
        p.error('invalid cohort set')
    if sum((s / 'results.json').stat().st_size for s in cohorts) > 64 * 1024**2:
        raise ValueError('cohort JSON byte budget')
    checked = CheckedFiles(); validation = a.validation.resolve()
    reference, proofs, bindings, comparison = validate_receipts(validation, checked)
    if not isinstance(comparison, str): raise ValueError('invalid comparison table')
    primary = {'blocks': [], 'identity': {'schedule': []}}; reports = []; samples = 0
    primary_designs = set()
    for path in cohorts:
        data, report, count = validate_cohort(path.resolve(), reference, proofs, checked)
        if path.name in {'repair-direct-vex', 'other-pdk-paired'} and report['status'] == 'regression_detected':
            raise ValueError('resolve repaired-producer control regression')
        samples += count
        if samples > 4096: raise ValueError('publication sample budget')
        reports.append({'cohort': path.name, 'primary': path in a.primary,
                        'measurements': data, 'statistics': report})
        if path in a.primary:
            designs = {(b['id'], b['library']) for b in data['blocks']}
            if designs & primary_designs: raise ValueError('duplicate primary design cohort')
            primary_designs.update(designs)
            primary['blocks'].extend(data['blocks']); primary['identity']['schedule'].extend(data['identity']['schedule'])
    expected = {(did, 'gf180mcu7t_tt') for did in scaling_report.ORDER}
    if {(b['id'], b['library']) for b in primary['blocks']} != expected:
        raise ValueError('primary cohort must cover all five designs')
    statistics = paired_statistics.analyze(primary)
    if statistics['status'] == 'regression_detected': raise ValueError('resolve confirmed regression before publication')
    output = a.output.resolve(); output.mkdir(parents=True, exist_ok=False)
    links = {}
    for name, value in [('statistics', statistics), ('all-cohorts', reports),
                        ('quality-and-lec', {'status': 'passed', 'revalidated_tool_pdk_netlists': 30,
                         'audited_samples': samples, 'fresh_solver_run': False, 'rows': bindings}),
                        ('checked-file-hashes', checked.checked)]:
        links[name] = scaling_report.packed_json(output / (name + '.json.gz'), value)
    for name in ('measurements', 'proofs', 'timing', 'quality'):
        links[name] = scaling_report.packed_json(output / (name + '.json.gz'), run.read_json(validation / (name + '.json')))
    archived = archive(output, [('audit', a.audit_root.resolve()), ('prior-validation', validation)])
    cpu = next(line.split(':', 1)[1].strip() for line in Path('/proc/cpuinfo').read_text().splitlines() if line.startswith('model name'))
    compiled_source = a.audit_root.resolve() / 'gf180-small/harness/precise_command.rs'
    timer_source = {}
    if compiled_source.is_file():
        current = run.ROOT / 'crates/verify/rustsys-check/examples/precise_command.rs'
        timer_source = {'compiled_source': str(compiled_source), 'sha256': run.digest(compiled_source),
                        'current_source_only_updates_memory_estimate_comment':
                        compiled_source.read_text().replace('<1 MiB state', '<4 MiB argv/state') == current.read_text()}
    run.save(output / 'provenance.json', {'host': {'cpu': cpu, 'kernel': os.uname().release, 'exclusive_host': False},
             'primary_cohorts': [str(p.resolve()) for p in a.primary],
             'supplemental_cohorts': [str(p.resolve()) for p in a.supplement],
             'reporter_sha256': run.digest(Path(__file__)), 'statistics_sha256': run.digest(Path(paired_statistics.__file__)),
             'published_samples': samples, 'native_timer_build_source': timer_source, **archived})
    body = table(statistics)
    initial_names = ('gf180-small', 'gf180-confirm', 'gf180-vex-clean')
    initial = {r['cohort']: r['statistics'] for r in reports if r['cohort'] in initial_names}
    diagnosis = ''
    if set(initial) == set(initial_names):
        means = [next(r['comparison']['paired_mean_delta_s'] for r in initial[name]['rows']
                      if r['id'] == 'vexriscv-verilog') * 1000 for name in initial_names]
        diagnosis = ('The initial 9afbf2b producer showed positive VexRiscv paired mean shifts '
                     f'of {means[0]:.3f}, {means[1]:.3f} and {means[2]:.3f} ms in three cohorts. '
                     'Their individual intervals were inconclusive. The persistent tendency and '
                     'redundant candidate work motivated the runtime repair; non-significance '
                     'was not treated as evidence of equality. A separate 32-pair 9afbf2b/repair '
                     'VexRiscv control and all initial cohorts remain in all-cohorts. '
                     'Different monitor profiles are not pooled into the primary result.\n')
    (output / 'table.md').write_text(body + '\n')
    (output / 'comparison.md').write_text(comparison + '\n')
    (output / 'README.md').write_text(f'''# GF180 alternating runtime audit

**CPU: {cpu}.** Fixed 16 physical cores, no SMT siblings; shared host.
Before = frozen pre-SERV-area producer; current = frozen runtime-repair producer.
All producer executable hashes are pinned in the complete cohort measurements.
Each cohort fixes its counts, seed and balanced A/B versus B/A order before running.
Two paired warm-ups are excluded. A/A controls execute the current binary on both sides.

{diagnosis}

{body}

Same-binary controls (reported separately; never subtracted from A/B):

{controls_table(statistics)}

Δ = current minus before; positive means slower. Before/current columns are
separate medians; Δ is the median of paired differences, so they need not subtract.
The 99% median intervals use exact binomial/order statistics. The machine-readable
results also retain paired means, seeded 10,000-replicate 99% bootstrap intervals,
log ratios, ranges, exact one-sided sign tests and Holm correction across five designs.
No confirmed regression was detected by the stated criterion. An interval containing
zero is inconclusive; it does not prove exact equality or exclude smaller regressions.
Read the absolute interval widths before interpreting a 0.01 s difference, particularly
for OpenC910. All exploratory and confirmation cohorts are retained separately.

The native monotonic timer wraps the unchanged complete CLI pipeline, including
Python startup/shutdown, all four CLI startups, initialization, RTL/Liberty import,
mmap, synthesis and full Verilog export. Resource-guard startup, source preparation,
LEC and OpenSTA are outside the clock; no overhead is subtracted. GNU time retains
CPU accounting and peak RSS, but its centisecond wall-time field is not used here.
Primary cohorts move campaign-wide disk scans outside the timer; OpenC910's bounded
periodic scanner checks only its current case. Earlier cohorts used a growing
campaign scan during timing and are labeled supplemental, never pooled silently.

Every timed netlist, including warm-ups and controls, is byte-identical to its
GF180 baseline/current implementation and has identical cell inventory and area.
Complete arbitrary-state RTL LEC and OpenSTA receipts are rebound by exact netlist,
RTL, library and top; all 30 retained tool/design/PDK certificates are revalidated.
This is explicit exact-byte reuse, not a fresh solver or STA run. No reset sequence,
input assumption or initial-state restriction is added. The runtime repair skips
unaffordable drive alternatives, reuses state-output pin roles and sink limits,
and avoids duplicate launch calculations. A balanced-truth precheck rejects
impossible mux functions. Candidate order, area ceilings and selected netlists
are preserved; the established three-PDK area/Fmax results remain unchanged.
The original two-valued data/X/Z completion, certified EZ130 nominal tie-cell scope,
and ideal-clock/zero-wire timing limitations remain in force. SERV/EZ130 and
OpenC910 raw setup-Fmax extrapolations do not establish achievable frequency.

{other_pdk_table(reports)}

## Three-PDK RustSys/Yosys refresh

One warm-up and three measured repetitions at 16 physical cores, original GNU-time
boundary. RustSys is freshly remeasured with the repaired producer; Yosys runs
retain their pinned original provenance. These times are not the paired audit
times above. Peak RSS is the maximum measured process peak across three runs.
Setup-Fmax retains the original ideal-clock, zero-wire, TT, same-edge register
scope and characterization/excluded-path limitations.

{comparison}

{' · '.join(f'[{n}]({v})' for n, v in links.items())} ·
[Raw clocks, commands, reports, frozen harnesses and resource receipts]({archived['receipts']}) ·
[Large artifact SHA-256 inventory]({archived['inventory']}) · [Provenance](provenance.json).
Original proof payloads remain in the
[SERV-area archive](../2026-10-23-serv-area/README.md),
[Sky130/GF180 archive](../2026-10-21-default-multicore/README.md) and
[EZ130 archive](../2026-10-23-ez130/README.md); reused receipts retain those source paths.
The RustSys/Yosys overview retains the original three-run protocol and timer
boundary; the alternating producer audit is a separate experiment.
''')
    (output / 'SHA256SUMS').write_text(''.join(f'{run.digest(f)}  {f.name}\n' for f in sorted(output.iterdir()) if f.is_file() and f.name != 'SHA256SUMS'))
    print(body, flush=True)


if __name__ == '__main__': main()
