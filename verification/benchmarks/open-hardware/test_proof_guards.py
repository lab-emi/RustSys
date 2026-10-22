# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Guard tests; synthetic graphs <100 cells, serial peak <32 MiB.

These are not the semantic oracle: external positive/corruption controls are
in validate_mac_context.py and validate_partition_comb.py.
"""
import tempfile
from pathlib import Path
import unittest
import mac_context
import native_comb
import run


class ProofGuardTests(unittest.TestCase):
    def test_native_refuses_temporal_state_and_initial_constraints(self):
        with tempfile.TemporaryDirectory() as name:
            path=Path(name)/'model.json'
            for typ in ('$dff','$dlatch','$assume','$anyseq','$initstate'):
                run.save(path,{'modules':{'m':{'cells':{'bad':{'type':typ}},'netnames':{},'ports':{}}}})
                with self.assertRaises(ValueError):native_comb.model(path,'m')
            run.save(path,{'modules':{'m':{'cells':{},'netnames':{'q':{'attributes':{'init':'0'}}},'ports':{}}}})
            with self.assertRaises(ValueError):native_comb.model(path,'m')

    def test_live_hint_traversal_has_hard_work_budget(self):
        m={'ports':{'y':{'direction':'output','bits':[2]}},'netnames':{},'cells':{
            'n':{'connections':{'A':[1],'Y':[2]},'port_directions':{'A':'input','Y':'output'}}}}
        with self.assertRaises(ValueError):native_comb.live_hint_names(m,max_expansions=0)

    def test_mac_requires_an_explicit_success_verdict(self):
        self.assertTrue(mac_context.successful('[INFO ] Verification success!\n'))
        for text in ('','exit status: 0','[INFO ] Worker1 finished first.\n',
                     '[INFO ] Verification failed!\n','[INFO ] Verification success!\nVerification failed'):
            self.assertFalse(mac_context.successful(text))

    def test_mac_refuses_hidden_inputs_unknowns_and_storage_in_cones(self):
        base={'ports':{'a':{'direction':'input','bits':[1]}},'cells':{},'netnames':{}}
        vectors={'a':[1],'b':[2],'c':[3],'y':[4]}
        with self.assertRaises(ValueError):mac_context.cone(base,vectors)
        base['cells']['r']={'type':'$_DFF_P_','connections':{'D':[1],'C':[1],'Q':[4]},
                           'port_directions':{'D':'input','C':'input','Q':'output'}}
        with self.assertRaises(ValueError):mac_context.cone(base,vectors)
        base['cells']['r']={'type':'$_NOT_','connections':{'A':['x'],'Y':[4]},
                           'port_directions':{'A':'input','Y':'output'}}
        with self.assertRaises(ValueError):mac_context.drivers(base)


if __name__=='__main__':unittest.main()
