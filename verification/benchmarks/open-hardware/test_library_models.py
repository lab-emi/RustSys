# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Parser guards; actual dual-PDK positive/negative proofs are external controls."""
from pathlib import Path
import tempfile
import unittest
import library_models

LIB = '''library (test) { cell (ffdual) {
 ff (IQ, IQ_N) { clocked_on : "CLK"; next_state : "D"; %s }
 pin (CLK) { direction : "input"; }
 pin (D) { direction : "input"; }
 pin (Q) { direction : "output"; function : "IQ"; }
 pin (QN) { direction : "output"; function : "IQ_N"; }
} }'''


class PhysicalFlipFlopModels(unittest.TestCase):
    def make(self, extra='', instance='ffdual r (.CLK(c), .D(d), .Q(q), .QN(qn));'):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        lib, net = root/'input.lib', root/'net.v'
        lib.write_text(LIB % extra)
        net.write_text(instance)
        return root, lib, net

    def test_complementary_outputs_have_one_uninitialized_state(self):
        root, lib, net = self.make()
        path, manifest = library_models.generate(lib, net, root)
        self.assertEqual(len(manifest['models']), 1)
        self.assertEqual(path.read_text().count('reg '), 1)
        self.assertNotIn('initial ', path.read_text())
        self.assertIn('assign QN = ~__lec_physical_state;', path.read_text())

    def test_unsupported_dual_state_control_fails_explicitly(self):
        root, lib, net = self.make('clear : "!D";')
        with self.assertRaisesRegex(ValueError, 'unsupported complementary-output'):
            library_models.generate(lib, net, root)

    def test_unused_unsupported_cell_is_recorded(self):
        root, lib, net = self.make('clear : "!D";', 'module t; endmodule')
        _, manifest = library_models.generate(lib, net, root)
        self.assertEqual(manifest['models'], [])
        self.assertEqual(manifest['unsupported_unused_dual_cells'], ['ffdual'])

    def test_reserved_state_identifier_is_rejected(self):
        root, lib, net = self.make()
        lib.write_text(lib.read_text().replace('pin (D)', 'pin (__lec_physical_state)').replace('"D"', '"__lec_physical_state"'))
        with self.assertRaisesRegex(ValueError, 'reserved physical-state'):
            library_models.generate(lib, net, root)
