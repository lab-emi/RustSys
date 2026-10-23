# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Probability/timer orchestration controls only, <1 MiB per test."""
import unittest

import paired_statistics as stats
import paired_audit


class PairedStatisticsTests(unittest.TestCase):
    def test_exact_sign_probability_and_holm(self):
        self.assertEqual(stats.sign_pvalue([1] * 10), 1 / 1024)
        self.assertEqual(stats.sign_pvalue([-1] * 10), 1.0)
        self.assertEqual(stats.sign_pvalue([0] * 10), 1.0)
        self.assertEqual(stats.holm({'a': 0.001, 'b': 0.04}, 2), {'a': 0.002, 'b': 0.04})

    def test_constant_slowdown_is_detected_and_balanced_control_contains_zero(self):
        slow = stats.summary([(1.0, 1.03)] * 20, 7)
        self.assertGreater(slow['paired_geomean_ratio_99pct_ci'][0], 1.0)
        control = stats.summary([(1.0, 1.002), (1.0, 0.998)] * 10, 7)
        lo, hi = control['paired_mean_delta_99pct_ci_s']
        self.assertLess(lo, 0); self.assertGreater(hi, 0)

    def test_exact_median_interval_requires_enough_observations(self):
        self.assertIsNone(stats.median_interval([1.0, 2.0]))
        self.assertEqual(stats.median_interval(list(range(1, 13))), [2, 11])
        self.assertEqual(stats.median_interval([0.01] * 40), [0.01, 0.01])

    def test_schedule_is_predeclared_balanced_and_bounded(self):
        rows = paired_audit.plan(['serv', 'ibex'], ['gf180mcu7t_tt'], 20, 2, 4, 9)
        for did in ['serv', 'ibex']:
            measured = [r for r in rows if r['id'] == did and r['kind'] == 'comparison']
            self.assertEqual(len(measured), 20)
            self.assertEqual(sum(r['order'][0] == 'baseline' for r in measured), 10)
        with self.assertRaises(ValueError):
            paired_audit.plan(['serv'] * 1000, ['gf180mcu7t_tt'], 64, 2, 4, 9)

    def test_missing_pair_failed_child_and_incomplete_schedule_are_rejected(self):
        plan = paired_audit.plan(['serv'], ['gf180mcu7t_tt'], 4, 1, 2, 9)
        blocks = [{**row, 'samples': [
            {'label': label, 'sample': {'status': 'ok', 'precise_wall_s': 1.0}}
            for label in row['order']]} for row in plan]
        data = {'identity': {'schedule': plan}, 'blocks': blocks}
        with self.assertRaises(ValueError):
            stats.analyze({**data, 'blocks': blocks[:-1]})
        blocks[0]['samples'].pop()
        with self.assertRaises(ValueError):
            stats.analyze(data)
        blocks[0]['samples'].append({'label': blocks[0]['order'][1],
                                     'sample': {'status': 'timeout', 'precise_wall_s': 1.0}})
        blocks[1]['samples'][0]['sample']['status'] = 'timeout'
        with self.assertRaises(ValueError):
            stats.analyze(data)


if __name__ == '__main__': unittest.main()
