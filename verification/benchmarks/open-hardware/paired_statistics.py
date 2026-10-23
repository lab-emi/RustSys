#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Bounded paired runtime inference; no EDA correctness claims.

<=64 MiB input, <=1024 pairs, five designs/three libraries. 10000 seeded
bootstrap replicates, <=64 draws each, one worker, <512 MiB planned RSS.
Exact one-sided sign tests with Holm correction. A non-significant result is
inconclusive, never proof of equality. A/A controls are reported separately;
their overhead/noise is not subtracted from A/B observations.
"""
from pathlib import Path
import argparse
import math
import random
import statistics

import run


def sign_pvalue(values):
    values = [v for v in values if v != 0]
    n = len(values); k = sum(v > 0 for v in values)
    if not n: return 1.0
    return sum(math.comb(n, j) for j in range(k, n + 1)) / (2**n)


def holm(values, family_size):
    if family_size < len(values): raise ValueError('multiple-test family is incomplete')
    result = {}; previous = 0.0
    for rank, (key, value) in enumerate(sorted(values.items(), key=lambda p: p[1])):
        previous = max(previous, min(1.0, (family_size - rank) * value))
        result[key] = previous
    return result


def median_interval(values, confidence=0.99):
    """Binomial/order-statistic interval, without normality or symmetry.

    Return None if even the observed min/max cannot attain the requested
    coverage. In particular, two A/A pairs cannot provide a 99% interval.
    """
    n = len(values)
    if not 1 <= n <= 64: raise ValueError('median interval work budget')
    k = 0; tail = 0
    for candidate in range(1, n // 2 + 1):
        tail += math.comb(n, candidate - 1)
        if 2 * tail / (2**n) <= 1 - confidence: k = candidate
        else: break
    if not k: return None
    ordered = sorted(values)
    return [ordered[k - 1], ordered[n - k]]


def interval(values, seed, confidence=0.99, replicates=10000):
    if not 2 <= len(values) <= 64 or replicates > 20000: raise ValueError('bootstrap work budget')
    rng = random.Random(seed); n = len(values)
    means = sorted(sum(values[rng.randrange(n)] for _ in range(n)) / n for _ in range(replicates))
    alpha = (1 - confidence) / 2
    return [means[int(alpha * replicates)], means[min(replicates - 1, math.ceil((1 - alpha) * replicates) - 1)]]


def summary(pairs, seed):
    if not 2 <= len(pairs) <= 64: raise ValueError('paired sample budget')
    if any(not math.isfinite(v) or v <= 0 for p in pairs for v in p): raise ValueError('invalid duration')
    old = [p[0] for p in pairs]; new = [p[1] for p in pairs]
    differences = [b - a for a, b in pairs]; logs = [math.log(b / a) for a, b in pairs]
    delta = interval(differences, seed); ratio = [math.exp(v) for v in interval(logs, seed + 1)]
    median_log = median_interval(logs)
    return {'pairs': len(pairs), 'baseline_median_s': statistics.median(old),
            'candidate_median_s': statistics.median(new), 'baseline_range_s': [min(old), max(old)],
            'candidate_range_s': [min(new), max(new)], 'paired_mean_delta_s': statistics.mean(differences),
            'paired_median_delta_s': statistics.median(differences),
            'paired_median_delta_99pct_exact_ci_s': median_interval(differences),
            'paired_median_ratio': math.exp(statistics.median(logs)),
            'paired_median_ratio_99pct_exact_ci': [math.exp(v) for v in median_log] if median_log else None,
            'paired_mean_delta_99pct_ci_s': delta, 'paired_geomean_ratio': math.exp(statistics.mean(logs)),
            'paired_geomean_ratio_99pct_ci': ratio, 'positive_differences': sum(v > 0 for v in differences),
            'one_sided_sign_p': sign_pvalue(differences)}


def analyze(data, family_size=5):
    schedule = data.get('identity', {}).get('schedule')
    if schedule is not None:
        if len(data['blocks']) != len(schedule): raise ValueError('predeclared audit is incomplete')
        for actual, planned in zip(data['blocks'], schedule):
            if any(actual[k] != planned[k] for k in ('kind', 'round', 'id', 'library', 'order')):
                raise ValueError('observations differ from predeclared schedule')
    groups = {}
    for block in data['blocks']:
        samples = {s['label']: s for s in block['samples']}
        if len(block['samples']) != 2 or set(samples) != {'baseline', 'candidate'}:
            raise ValueError('incomplete pair')
        for sample in samples.values():
            if sample['sample']['status'] != 'ok': raise ValueError('failed duration cannot be analyzed')
        if block['kind'] == 'warmup': continue
        values = tuple(samples[k]['sample']['precise_wall_s'] for k in ('baseline', 'candidate'))
        key = (block['id'], block['library'])
        groups.setdefault(key, {'comparison': [], 'same_binary': []})[block['kind']].append(values)
    rows = []
    for n, (key, groups_for_key) in enumerate(sorted(groups.items())):
        if len(groups_for_key['comparison']) < 4 or len(groups_for_key['same_binary']) < 2:
            raise ValueError('incomplete predeclared observations/control')
        control = summary(groups_for_key['same_binary'], 20361023 + 2 * n)
        if len(groups_for_key['same_binary']) < 4:
            control['paired_mean_delta_99pct_ci_s'] = None
            control['paired_geomean_ratio_99pct_ci'] = None
            control['interval_status'] = 'not_estimated; fewer than four control pairs, descriptive only'
        rows.append({'id': key[0], 'library': key[1],
                     'comparison': summary(groups_for_key['comparison'], 20261023 + 2 * n),
                     'same_binary_control': control})
    adjusted = holm({(r['id'], r['library']): r['comparison']['one_sided_sign_p'] for r in rows}, family_size)
    for r in rows:
        s = r['comparison']; s['holm_p'] = adjusted[r['id'], r['library']]
        lo, hi = s['paired_geomean_ratio_99pct_ci']
        median_ci = s['paired_median_delta_99pct_exact_ci_s']
        positive = lo > 1 or (median_ci is not None and median_ci[0] > 0)
        negative = hi < 1 or (median_ci is not None and median_ci[1] < 0)
        r['classification'] = ('regression' if positive and s['holm_p'] <= 0.05 else
                               'improvement' if negative else 'inconclusive')
    return {'method': 'paired-median-exact99-mean-bootstrap99-sign-holm-v2', 'family_size': family_size,
            'bootstrap_replicates': 10000, 'seed': 20261023, 'rows': rows,
            'status': 'regression_detected' if any(r['classification'] == 'regression' for r in rows) else 'no_regression_detected',
            'interpretation': 'Inconclusive does not establish equality or non-regression. Full paired samples and A/A controls remain part of the result.'}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--input', type=Path, required=True); p.add_argument('--output', type=Path, required=True)
    p.add_argument('--family-size', type=int, default=5)
    a = p.parse_args()
    if a.input.stat().st_size > 64 * 1024**2: raise ValueError('JSON byte budget')
    result = analyze(run.read_json(a.input), a.family_size)
    run.save(a.output, result)
    for row in result['rows']:
        c = row['comparison']
        print(row['id'], row['library'], row['classification'], c['pairs'],
              f"{1000 * c['paired_mean_delta_s']:.3f} ms", c['paired_geomean_ratio_99pct_ci'], c['holm_p'])


if __name__ == '__main__': main()
