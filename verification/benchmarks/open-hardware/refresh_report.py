#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Publish a fully validated 16-core refresh without rewriting historical runs.

<=15 rows and 64 MiB JSON, <512 MiB reporter RSS. Streaming evidence archive
uses the existing 65536-file/256-MiB receipt budgets. No graph or solver runs.
Run with a 2 GiB address-space guard and a 600 s wall limit.
"""
from pathlib import Path
import argparse
import math
import os

import additional_report
import additional_verify
import functional_models
import observed_proof
import refresh_campaign
import run
import scaling_report
import scaling_verify

LABELS = {**scaling_report.PDK, 'ez130_8t_tt': 'EZ130 8T'}


def comparison(data, proofs, timing):
    rows = refresh_campaign.matrix(data)
    p = {(r['id'], r['library']): r for r in proofs['rows']}
    t = {(r['id'], r['library']): r for r in timing['rows']}
    if len(proofs['rows']) != 15 or len(timing['rows']) != 15 or set(p) != set(rows) or set(t) != set(rows):
        raise ValueError('incomplete proof/timing matrix')
    columns = ['Design', 'PDK', 'RustSys<br>16 cores, s ↓', 'Yosys<br>16 cores, s ↓',
               'RustSys<br>Speedup ↑', 'RustSys<br>Peak mem, MiB ↓', 'Yosys<br>Peak mem, MiB ↓',
               'RustSys<br>Area, µm² ↓', 'Yosys<br>Area, µm² ↓',
               'RustSys<br>Setup Fmax†, MHz ↑', 'Yosys<br>Setup Fmax†, MHz ↑']
    lines = ['| ' + ' | '.join(columns) + ' |', '|---|---|' + '---:|' * 9]
    for did in scaling_report.ORDER:
        for lib in refresh_campaign.LIBRARIES:
            row = rows[did, lib]
            for tool in ('rustsys', 'yosys'):
                value = row[tool]; proof = p[did, lib][tool]; sta = t[did, lib][tool]
                digest = value['netlist_sha256']
                if (not scaling_verify.complete(proof) or sta.get('status') != 'ok'
                        or digest not in proof.get('source_sha256', {}).values()
                        or digest not in sta.get('sha256', {}).values()
                        or len(value.get('samples', [])) != 3
                        or any(s['status'] != 'ok' or s['netlist_sha256'] != digest for s in value['samples'])):
                    raise ValueError('unproven or unbound measurement')
                numbers = [value['median_wall_s'], value['qor']['area_um2'], sta['fmax_mhz']]
                if any(type(x) not in (int, float) or not math.isfinite(x) or x <= 0 for x in numbers):
                    raise ValueError('invalid time, area or Fmax')
            speedup = row['yosys']['median_wall_s'] / row['rustsys']['median_wall_s']
            values = [row['design'], LABELS[lib]]
            values += [f"{row[tool]['median_wall_s']:,.2f}" for tool in ('rustsys', 'yosys')]
            values += [f'**{speedup:.2f}×**']
            values += [scaling_report.peak_memory_mib(row[tool]) for tool in ('rustsys', 'yosys')]
            values += [f"{row[tool]['qor']['area_um2']:,.0f}" for tool in ('rustsys', 'yosys')]
            values += [scaling_report.frequency(t[did, lib][tool]['fmax_mhz']) for tool in ('rustsys', 'yosys')]
            lines.append('| ' + ' | '.join(values) + ' |')
    return '\n'.join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('workspace', 'validation', 'controls', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--update-docs', action='store_true')
    args = parser.parse_args()
    ws = args.workspace.resolve(); validation = args.validation.resolve(); output = args.output.resolve()
    completion = run.read_json(validation / 'complete-all.json')
    if completion.get('status') != 'complete' or completion['measurements_sha256'] != run.digest(ws / 'results.json'):
        raise ValueError('validation does not bind the current campaign')
    if any((validation / name).stat().st_size > 64 * 1024**2 for name in ('proofs.json', 'timing.json', 'measurements.json')):
        raise ValueError('publication JSON byte budget')
    for name, digest in completion['artifacts'].items():
        if run.digest(validation / name) != digest: raise ValueError('validation artifact changed')
    data = run.read_json(validation / 'measurements.json')
    proofs = run.read_json(validation / 'proofs.json'); timing = run.read_json(validation / 'timing.json')
    table = comparison(data, proofs, timing)
    additional_verify.check_hashes({**data['input_sha256'], **data['binary_sha256'], **data['source_campaigns']})
    for rows, key in ((proofs['rows'], 'source_sha256'), (timing['rows'], 'sha256')):
        for row in rows:
            for tool in ('rustsys', 'yosys'):
                value = row[tool]
                additional_verify.check_hashes(value[key])
                additional_verify.check_hashes(value.get('artifact_sha256', {}))
                additional_verify.check_hashes(value.get('receipts_sha256', {}))
                for record in value.get('tools', {}).values():
                    if isinstance(record, dict) and 'path' in record and 'sha256' in record:
                        additional_verify.check_hashes({record['path']: record['sha256']})
                proof_comparison = value.get('certificate', {}).get('comparison', {})
                additional_verify.check_hashes(proof_comparison.get('source_sha256', {}))
                additional_verify.check_hashes(proof_comparison.get('tool_sha256', {}))
                if value.get('method') == observed_proof.METHOD: observed_proof.validate(value)
                supplement = value.get('functional_library_supplement')
                if supplement:
                    if run.digest(supplement['certificate']) != supplement['certificate_sha256']:
                        raise ValueError('functional-model supplement changed')
                    functional_models.validate(run.read_json(Path(supplement['certificate'])))
    for row in data['rows']:
        cfg = additional_verify.checked_config(ws, row, 'rustsys')
        expected = {run.digest(p) for p in [cfg['library'], *cfg['design']['files']]}
        entry = next(r for r in proofs['rows'] if r['id'] == row['id'] and r['library'] == row['library'])
        for tool in ('rustsys', 'yosys'):
            if not expected <= set(entry[tool]['source_sha256'].values()):
                raise ValueError('proof does not bind the measured RTL/library')
    output.mkdir(parents=True, exist_ok=False)
    artifacts = {name: scaling_report.packed_json(output / (name + '.json.gz'), value)
                 for name, value in [('measurements', data), ('proofs', proofs), ('timing', timing),
                                                    ('quality', run.read_json(validation / 'quality.json'))]}
    evidence, inventory, stats = additional_report.archive_evidence(output, ws, validation, args.controls)
    cpu = next(line.split(':', 1)[1].strip() for line in Path('/proc/cpuinfo').read_text().splitlines() if line.startswith('model name'))
    run.save(output / 'provenance.json', {'host': {'cpu': cpu, 'kernel': os.uname().release, 'exclusive_host': False},
             'reporter_sha256': run.digest(Path(__file__)), 'source_workspace': str(ws),
             'validation_workspace': str(validation), 'original_campaigns': data['source_campaigns'],
             'scope': 'new RustSys 16-core measurements; retained pinned Yosys measurements; exact-byte proof/timing reuse explicitly identified', **stats})
    links = ' · '.join(f'[{name.title()}]({path})' for name, path in artifacts.items())
    (output / 'README.md').write_text(f'''# Validated 16-core synthesis refresh

**CPU: {cpu}.** 16 physical host cores per tool, without SMT siblings.
One warm-up and three measured runs; median end-to-end seconds, maximum process RSS.
Time includes initialization/import, synthesis, mmap and full Verilog export.
Source preparation, LEC and OpenSTA run outside the timer.

{table}

RustSys was remeasured for all fifteen design/PDK combinations. Yosys measurements
are retained from the pinned reference campaigns listed in provenance.json;
no new Yosys timing run is claimed. Changed netlists require fresh complete LEC
and OpenSTA; exact-byte reuse checks RTL, library, top, tools and original receipts.
All thirty tool/design/PDK entries have complete arbitrary-state RTL LEC.
No reset sequence or input assumptions are added. Ordinary X/Z data and undriven
wires use the existing explicit zero-completion contract, without initializing state.

EZ130 functional LEC retains the certified nominal vendor tie-cell interpretation.
The two conflicting Liberty driver-type attributes are recorded in the supplement;
the original synthesis/timing library is unchanged. Strength/high-impedance behavior
is outside this functional proof. OpenSTA Fmax is an ideal-clock, zero-wire,
same-edge register setup estimate. Large OpenC910 loads exceed characterization;
these estimates do not establish achievable or signoff frequency.

{links} · [Receipts]({evidence}) · [Large artifact hashes]({inventory}) · [Provenance](provenance.json).
Historical 1/16-core measurements remain in their original archives.
''')
    if args.update_docs:
        for name, begin, end in [('README.md', scaling_report.START, scaling_report.END),
                                ('docs/index.md', '<!-- benchmark-16-core:start -->', '<!-- benchmark-16-core:end -->'),
                                ('docs/guide/benchmarks.md', '<!-- benchmark-16-core:start -->', '<!-- benchmark-16-core:end -->')]:
            path = run.ROOT / name
            path.write_text(scaling_report.block(path.read_text(), begin, end, table))
    print(output, flush=True)


if __name__ == '__main__': main()
