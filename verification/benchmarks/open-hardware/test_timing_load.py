# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Load-scope observations: tiny bound receipts, <64 MiB, no EDA process."""
from pathlib import Path
import tempfile
import unittest

import inspect_timing_load
import run


class TimingLoadTests(unittest.TestCase):
    def test_quoted_and_bare_liberty_use_the_same_physical_units(self):
        for quoted in (True,False):
            with self.subTest(quoted=quoted),tempfile.TemporaryDirectory() as directory:
                d=Path(directory);lib=d/'cells.lib';path=d/'worst-path.json'
                q=lambda value:'"'+value+'"' if quoted else value
                lib.write_text('library (L) { capacitive_load_unit (1,'+q('pf')+'); cell ('+q('X')+') { pin ('+q('Y')+') { max_capacitance : 0.2; timing () { cell_rise (table) { index_2 ("0.001, 0.1"); } } } } }')
                run.save(path,{'checks':[{'path_group':'bench_clk','source_clock':'clk','target_clock':'clk',
                    'source_clock_edge':'rise','target_clock_edge':'rise','source_path':[
                        {'instance':'u','cell':'X','pin':'u/Y','net':'n','arrival':2e-9,'capacitance':0.3e-12}]}]})
                timing={'receipts_sha256':{str(path):run.digest(path)},'sha256':{str(lib):run.digest(lib)}}
                result=inspect_timing_load.observe(timing,'tool')
                self.assertAlmostEqual(result['load_pf'],0.3)
                self.assertEqual(result['cell_max_capacitance_pf'],0.2)
                self.assertEqual(result['largest_delay_table_load_pf'],0.1)
                self.assertTrue(result['outside_every_delay_table_load_axis'])
                lib.write_text(lib.read_text()+'\nchanged')
                with self.assertRaisesRegex(ValueError,'timing input changed'):
                    inspect_timing_load.observe(timing,'tool')

    def test_group_budget_prevents_unbounded_metadata(self):
        with self.assertRaisesRegex(ValueError,'group budget'):
            inspect_timing_load.groups('cell','cell(A) {} cell(B) {} cell(C) {}',2)


if __name__=='__main__':unittest.main()
