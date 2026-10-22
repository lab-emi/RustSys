# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""LEC metadata guards; one worker and less than 64 MiB expected RSS."""
from pathlib import Path
import tempfile
import unittest

import equiv
import lec


class LecMetadataTests(unittest.TestCase):
    def test_internal_memory_metadata_keeps_ports_and_scalar_bit_names(self):
        source = ('module top(a,q);\ninput a;\noutput q;\n'
                  'reg [0:0] \\$private_memory  [63:0];\n'
                  'reg [7:0] data [31:0];\nwire \\state[4] ;\nendmodule\n')
        metadata = lec.internal_metadata_text(source)
        self.assertNotIn('$private_memory', metadata)
        self.assertNotIn('data [31:0]', metadata)
        self.assertIn('wire \\state[4] ;', metadata)
        self.assertEqual(lec.port_shapes(metadata, 'top'), {'a': ('input', 1), 'q': ('output', 1)})

    def test_bitwise_output_alias_keeps_only_one_independent_cut(self):
        model = {"modules": {"top": {"ports": {
            "q[0]": {"direction": "output", "bits": [2]},
            "mirror": {"direction": "output", "bits": [2]}}, "netnames": {
            "q[0]": {"bits": [2]}, "mirror": {"bits": [2]}, "state[0]": {"bits": [2]}}}}}
        self.assertEqual(lec.correspondence_bindings(model, model, "top"), {"mirror", "state[0]"})

    def test_one_sided_state_merging_keeps_the_other_driver_in_scope(self):
        gold = {"modules": {"top": {"ports": {}, "netnames": {
            "a": {"bits": [2]}, "b": {"bits": [3]}}}}}
        gate = {"modules": {"top": {"ports": {}, "netnames": {
            "a": {"bits": [7]}, "b": {"bits": [7]}}}}}
        self.assertEqual(lec.correspondence_bindings(gold, gate, "top"), {"b"})

    def test_state_cut_is_preferred_to_its_output_alias(self):
        model = {"modules": {"top": {"ports": {
            "out": {"direction": "output", "bits": [2]}}, "netnames": {
            "out": {"bits": [2]},
            "state": {"bits": [2], "attributes": {"lec_state": "1"}}}}}}
        self.assertEqual(lec.correspondence_bindings(model, model, "top"), {"out"})

    def test_real_register_name_precedes_shorter_functional_alias(self):
        model = {"modules": {"top": {"ports": {}, "netnames": {
            "short": {"bits": [2], "attributes": {"lec_state": "1"}},
            "actual_register[20]": {"bits": [2], "attributes": {"lec_state": "1"}}}}}}
        bound = lec.correspondence_bindings(model, model, "top", preferred={"actual_register[20]"})
        self.assertEqual(bound, {"short"})
        self.assertEqual(lec.register_names("reg [4:2] q;\n", "top"), {"q[2]", "q[3]", "q[4]"})
        with self.assertRaises(equiv.AdaptationError):
            lec.register_names("reg [1000000000000:0] q;\n", "top")

    def test_rustsys_port_positions_are_normalized_after_bitwise_lowering(self):
        commands = equiv.renames("output [4:2] mirror;\n", "top", include_ports=True)
        self.assertIn("rename mirror[4]", commands)
        self.assertIn(" mirror[0]", commands)

    def test_proof_ports_do_not_collide_with_exported_bit_wires(self):
        text = "module top;\ninput clk;\noutput [2:0] mirror;\nwire \\mirror[0] ;\nendmodule\n"
        mapping, commands = lec.port_namespace((text, text), "top")
        self.assertEqual(set(mapping), {"clk", "mirror"})
        self.assertEqual(len(set(mapping.values())), 2)
        self.assertNotIn("mirror[0]", commands)
        changed = lec.renamed_port_metadata(text, mapping)
        self.assertIn("wire \\mirror[0]", changed)
        self.assertIn("output [2:0] " + mapping["mirror"], changed)

    def test_yosys_only_normalizes_zero_index_single_bit_vectors(self):
        commands = equiv.renames("wire [0:0] valid;\nwire [15:8] data;\n", "top",
                                 zero_scalar_only=True)
        self.assertIn("rename valid[0]", commands)
        self.assertNotIn("data[", commands)

    def test_rule_expansion_is_bounded_before_building_text(self):
        self.assertEqual(lec.name_pattern(["state[0]"]), r"/^(?:state\[0\])$/")
        with self.assertRaises(equiv.AdaptationError):
            lec.name_pattern(["long_name"], max_bytes=8)

    def test_aliases_and_optimized_constants_retain_their_drivers(self):
        def model(alias, constant):
            return {"modules": {"top": {"ports": {"out": {"direction": "output", "bits": [2]}}, "netnames": {
                "out": {"bits": [2]}, "state_alias": {"bits": [alias]},
                "state_bit": {"bits": [constant]}, "independent": {"bits": [4]},
            }}}}
        bindings = lec.correspondence_bindings(model(2, 3), model(2, "0"), "top")
        self.assertEqual(bindings, {"state_alias", "state_bit"})
        self.assertNotIn("out", bindings)

    def test_connectivity_metadata_uses_bit_positions_and_has_a_budget(self):
        model = {"modules": {"top": {"ports": {}, "netnames": {
            "ascending": {"bits": [2, 3, 4], "offset": 2, "upto": 1}}}}}
        names, _ = lec.metadata_names(model, "top")
        self.assertEqual(names, {"ascending[0]": (2,), "ascending[1]": (3,),
                                 "ascending[2]": (4,)})
        with self.assertRaises(equiv.AdaptationError):
            lec.metadata_names(model, "top", max_bits=2)

    def test_a_constant_bit_does_not_bind_an_entire_wide_output(self):
        model = {"modules": {"top": {"ports": {
            "bus": {"direction": "output", "bits": [2, 3, "0"]}}, "netnames": {
            "bus": {"bits": [2, 3, "0"]}, "state_alias": {"bits": [2]}}}}}
        self.assertEqual(lec.correspondence_bindings(model, model, "top"), {"state_alias"})

    def test_word_groups_are_disjoint_and_bounded(self):
        names = [f"ram[3][{i}]" for i in range(70)]
        groups = lec.word_groups(names, 32)
        self.assertEqual(sorted(map(len, groups)), [6, 32, 32])
        self.assertEqual(set().union(*map(set, groups)), set(names))
        self.assertEqual(lec.word_groups(names, 0), [])
        with self.assertRaises(equiv.AdaptationError):
            lec.word_groups(names, 32, max_groups=2)

    def test_clock_sampling_changes_only_generated_strategy_inputs(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            path = root / "strategies/partition/pdr/check.sby"
            path.parent.mkdir(parents=True)
            path.write_text("[script]\nread_rtlil partition.il\nasync2sync\nformalff -clk2ff gate\n")
            manifest = lec.partition_clock_scripts(root)
            self.assertEqual(len(manifest), 1)
            self.assertIn("clk2fflogic\nscc -expect 0\nformalff", path.read_text())
            self.assertNotEqual(manifest[0]["before_sha256"], manifest[0]["after_sha256"])
            with self.assertRaises(equiv.AdaptationError):
                lec.partition_clock_scripts(root)

    def test_uncalibrated_native_sat_cannot_produce_a_certificate(self):
        with tempfile.TemporaryDirectory() as name:
            proof = lec.prove({}, Path(name), "unused", "unused", "unused", strategy="sat-pdr-smt")
            self.assertEqual(proof["verdict"], "unverified")
            self.assertIn("output-corruption", proof["diagnostic"]["message"])

    def test_writer_temporaries_are_not_renamed_in_the_direct_model(self):
        text = ("wire [1023:0] _0_ = {a, b};\nreg [4:2] q;\n"
                "wire [3:0] \\core.func$path/to/input.tmp ;\n")
        commands = lec.direct_model_names(text, "top", None)
        self.assertNotIn("_0_[", commands)
        self.assertNotIn("func$", commands)
        self.assertIn("rename q[2]", commands)
        self.assertIn("rename q[4]", commands)

    def test_ports_are_collected_and_an_empty_observation_is_rejected(self):
        self.assertEqual(lec.output_names("output [3:0] result;\ninput clk;\noutput ready;\n"),
                         ["ready", "result"])
        with self.assertRaises(equiv.AdaptationError):
            lec.output_names("input clk;\nwire data;\n")

    def test_generated_output_size_and_count_are_bounded(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            (root / "a").write_bytes(b"1234")
            (root / "b").write_bytes(b"56")
            lec.tree_budget(root, max_bytes=6, max_files=2)
            with self.assertRaises(equiv.AdaptationError):
                lec.tree_budget(root, max_bytes=5)
            with self.assertRaises(equiv.AdaptationError):
                lec.tree_budget(root, max_files=1)

    def test_only_direct_input_aliases_are_bound(self):
        text = ("input [2:0] data;\noutput alias;\noutput computed;\n"
                "assign alias = data[2];\nassign computed = data[0] & data[1];\n")
        self.assertEqual(lec.input_alias_outputs(text), {"alias"})

    def test_interface_width_and_direction_match_by_bit_position(self):
        original = lec.port_shapes("input clk;\noutput [4:2] data;\n")
        for bad in ("input clk;\noutput [3:2] data;\n",
                    "input clk;\ninput [4:2] data;\n"):
            self.assertNotEqual(original, lec.port_shapes(bad))
        self.assertEqual(original, lec.port_shapes("input clk;\noutput [2:0] data;\n"))

    def test_strategy_certificate_requires_every_nonempty_obligation(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            status = root / "strategies/p1/pdr/status"
            status.parent.mkdir(parents=True)
            status.write_text("PASS\n")
            results = lec.strategy_results(root, ["p1", "p2"])
            self.assertTrue(results["p1"]["proven"])
            self.assertFalse(results["p2"]["proven"])
            status.write_text("UNKNOWN\n")
            self.assertFalse(lec.strategy_results(root, ["p1"])["p1"]["proven"])
            status.write_text("PASS (cached)\n")
            sat = status.parent.parent / "sat/status"
            sat.parent.mkdir()
            sat.write_text("PASS\n")
            self.assertFalse(lec.strategy_results(root, ["p1"])["p1"]["proven"])
            for parts in ([], ["p1", "p1"], ["../outside"]):
                with self.assertRaises(equiv.AdaptationError):
                    lec.strategy_results(root, parts)

    def test_helper_function_arguments_are_not_module_ports(self):
        text = ("module helper;\ninput hidden;\nendmodule\n"
                "module top;\ninput clk;\noutput [2:0] result;\n"
                "function [2:0] mux;\ninput [7:0] a;\ninput [2:0] s;\nendfunction\n"
                "reg [4:2] q;\nendmodule\n")
        self.assertEqual(lec.port_shapes(text, "top"), {"clk": ("input", 1), "result": ("output", 3)})
        commands = equiv.renames(text, "top")
        self.assertNotIn(" a[", commands)
        self.assertNotIn(" s[", commands)
        self.assertIn(" q[4]", commands)


if __name__ == "__main__":
    unittest.main()
