# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Small orchestration controls; <1 MiB, no design graph or EDA oracle."""
import copy
import unittest

import refresh_campaign
import scaling_report


class RefreshMatrixTests(unittest.TestCase):
    def data(self):
        return {'limits': {'repetitions': 3, 'warmups': 1}, 'rows': [
            {'id': did, 'library': lib, 'allocated_physical_cores': 16,
             'cpu_affinity': list(range(16)), 'rustsys': {
                 'status': 'ok', 'netlist_sha256': 'frozen',
                 'samples': [{'status': 'ok', 'netlist_sha256': 'frozen'} for _ in range(3)]}}
            for did in scaling_report.ORDER for lib in refresh_campaign.LIBRARIES]}

    def test_complete_matrix_and_duplicate_rejection(self):
        data = self.data()
        self.assertEqual(len(refresh_campaign.matrix(data)), 15)
        data['rows'][-1] = copy.deepcopy(data['rows'][0])
        with self.assertRaises(ValueError): refresh_campaign.matrix(data)

    def test_missing_runs_nondeterminism_and_fake_core_allocation_fail(self):
        for kind in ('partial', 'changed', 'cores', 'warmup'):
            data = self.data(); row = data['rows'][0]
            if kind == 'partial': row['rustsys']['samples'].pop()
            if kind == 'changed': row['rustsys']['samples'][0]['netlist_sha256'] = 'changed'
            if kind == 'cores': row['cpu_affinity'] = [0] * 16
            if kind == 'warmup': data['limits']['warmups'] = 0
            with self.assertRaises(ValueError, msg=kind): refresh_campaign.matrix(data)


if __name__ == '__main__': unittest.main()
