# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Inventory acceptance guards; metadata only, <32 MiB RSS, no graph/solver."""
import unittest
import large_lec


class NativeInventoryTests(unittest.TestCase):
    def test_extra_quantified_rtl_bits_do_not_remove_implementation_obligations(self):
        manifest={'rtl_state_bits':2393,'gate_state_elements':1763}
        producer={'state_bits_rtl':2361,'state_bits_retained':1763}
        result=large_lec.verify_inventory(manifest,producer,False)
        self.assertTrue(result['retained_state_count_checked'])
        self.assertFalse(result['rtl_allocation_count_equal'])
        self.assertEqual(result['independent_rtl_state_bits'],2393)

    def test_retained_state_mismatch_fails_closed(self):
        for full in (False,True):
            with self.assertRaisesRegex(ValueError,'retained-state'):
                large_lec.verify_inventory({'rtl_state_bits':2393,'gate_state_elements':1762},
                    {'state_bits_rtl':2393,'state_bits_retained':1763},full)

    def test_full_core_inventory_contract_stays_strict(self):
        with self.assertRaisesRegex(ValueError,'full-core'):
            large_lec.verify_inventory({'rtl_state_bits':2393,'gate_state_elements':1763},
                {'state_bits_rtl':2361,'state_bits_retained':1763},True)


if __name__=='__main__':unittest.main()
