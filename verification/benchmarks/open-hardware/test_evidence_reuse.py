# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Receipt-binding controls only: <1MiB temporary data, no EDA claims/solver."""
from pathlib import Path
import tempfile
import unittest

import evidence_reuse
import run


class ReuseTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name)
        self.paths={n:self.root/n for n in ('rtl.v','cells.lib','old.v','new.v','sta','receipt.json')}
        for name,p in self.paths.items():p.write_text('netlist' if name in ('old.v','new.v') else name)
        self.key=('serv','sky130hd_tt','rustsys')
        self.cfg={'library':str(self.paths['cells.lib']),'design':{'top':'m','files':[str(self.paths['rtl.v'])]}}
        self.pins={str(self.paths[n]):run.digest(self.paths[n]) for n in ('rtl.v','cells.lib','old.v')}

    def index(self,value):
        return {self.key:[(self.paths['receipt.json'],run.digest(self.paths['receipt.json']),value)]}

    def test_proof_requires_identical_sources_and_a_complete_original(self):
        value={'source_sha256':self.pins,'status':'ok','scope':{'design_top':'m'}}
        checked=lambda p:p['status']=='ok'
        def reuse():return evidence_reuse.proof(self.index(value),self.key,self.cfg,self.paths['new.v'],checked)
        got=reuse();self.assertFalse(got['reuse_binding']['fresh_solver_run'])
        self.cfg['design']['top']='other';self.assertIsNone(reuse());self.cfg['design']['top']='m'
        self.assertEqual(got['source_sha256'][str(self.paths['new.v'])],run.digest(self.paths['new.v']))
        self.paths['new.v'].write_text('changed');self.assertIsNone(reuse())
        self.paths['new.v'].write_text('netlist')
        self.paths['rtl.v'].write_text('changed');self.assertIsNone(reuse())
        self.paths['rtl.v'].write_text('rtl.v')
        value['status']='unverified'
        with self.assertRaisesRegex(ValueError,'incomplete'):reuse()
        value['status']='ok';self.paths['old.v'].write_text('tampered retained input')
        with self.assertRaisesRegex(ValueError,'changed'):reuse()

    def test_timing_requires_the_same_tool_and_analysis_constraints(self):
        pins={str(self.paths[n]):run.digest(self.paths[n]) for n in ('cells.lib','old.v','sta')}
        scope={'clock':'bench_clk on clk','input_transition_ns':0.1,'output_load_pf':0.01,
            'input_output_delays_ns':0,'root_clock_period_ns':1000,
            'clock_model':'ideal;100ps transition;zero uncertainty',
            'wire_model':'none; no estimated or routed parasitics'}
        value={'status':'ok','fmax_mhz':1.0,'sha256':pins,'scope':scope}
        def reuse():return evidence_reuse.timing(self.index(value),self.key,self.paths['new.v'],
            self.paths['cells.lib'],self.paths['sta'],'clk')
        self.assertFalse(reuse()['reuse_binding']['fresh_sta_run'])
        scope['output_load_pf']=0.02;self.assertIsNone(reuse());scope['output_load_pf']=0.01
        self.paths['sta'].write_text('other tool');self.assertIsNone(reuse())
        self.paths['sta'].write_text('sta');value['fmax_mhz']=float('nan')
        with self.assertRaisesRegex(ValueError,'incomplete'):reuse()


if __name__=='__main__':unittest.main()
