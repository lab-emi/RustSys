# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Wire-name adapter guards; <64 MiB peak, no graph or solver workload."""
import unittest
import equiv


class CanonicalNamesTests(unittest.TestCase):
    def test_scalar_single_bit_and_both_range_directions(self):
        commands = equiv.renames(
            "input [4:2] port;\nwire [4:2] port;\nwire scalar;\n"
            "reg [0:0] carry;\nreg [4:2] count;\nreg [0:2] reverse;\n", "top")
        self.assertIn('carry[0]', commands)
        self.assertIn('carry', commands)
        self.assertIn('count[4]', commands)
        self.assertIn('count[0]', commands)
        self.assertIn('reverse[2]', commands)
        self.assertIn('reverse[0]', commands)
        self.assertNotIn('port[', commands)
        self.assertNotIn('scalar', commands)
        first_destination = commands.index('rename rustsys_proof_bit_')
        self.assertLess(commands.index('rename count[4]'), first_destination)

    def test_unpacked_element_names_keep_their_index(self):
        commands = equiv.renames("reg [0:0] \\bank[3] ;\n", "top")
        self.assertIn('bank[3][0]', commands)
        self.assertIn('bank[3]', commands)
        self.assertNotIn('bank[0]', commands)

    def test_yosys_wire_declaration_assignment_keeps_bit_positions(self):
        commands = equiv.renames("wire [4:2] part = {a, b, c};\nreg [0:0] q = 1'b0;\n", "top")
        self.assertIn("rename part[2]", commands)
        self.assertIn("rename part[4]", commands)
        self.assertIn("rename q[0]", commands)

    def test_collision_and_unhandled_declarations_are_rejected(self):
        for text in (
            "reg [1:0] value;\nwire \\value[0] ;\n",
            "reg memory [0:7];\n",
        ):
            with self.assertRaises(equiv.AdaptationError):
                equiv.renames(text, "top")

    def test_work_limits_are_enforced_before_materializing_bits(self):
        with self.assertRaises(equiv.AdaptationError):
            equiv.renames("wire [1000000000:0] value;\n", "top", max_bits=10)
        with self.assertRaises(equiv.AdaptationError):
            equiv.renames("wire a;\nwire b;\n", "top", max_declarations=1)
        with self.assertRaises(equiv.AdaptationError):
            equiv.renames("wire [4096:1] " + "long_name_" * 5000 + ";\n", "top")
        with self.assertRaises(equiv.AdaptationError):
            equiv.renames("wire a;\n", "top", deadline=0)

    def test_private_names_are_not_guessed_and_temporary_names_do_not_collide(self):
        commands = equiv.renames(
            "wire [2:0] \\$internal ;\nwire rustsys_proof_bit_0;\nreg [2:1] q;\n", "top")
        self.assertNotIn('$internal', commands)
        self.assertIn('_rustsys_proof_bit_0', commands)

    def test_already_canonical_bus_does_not_generate_identity_commands(self):
        commands = equiv.renames("reg [4095:0] q;\n", "top")
        self.assertEqual(commands, "cd top\ncd ..\n")


if __name__ == "__main__":
    unittest.main()
