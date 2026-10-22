# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Structural guards only; real positive/negative proofs live in validate_lec.py."""
import copy
from pathlib import Path
import tempfile
import unittest

import report
import transition


def fixture():
    return {
        "ports": {"clk": {"direction": "input", "bits": [2]},
                  "data": {"direction": "input", "bits": [3, 4]},
                  "q": {"direction": "output", "bits": [5]}},
        "cells": {"storage": {"type": "$dff", "parameters": {"CLK_POLARITY": "1"},
                    "port_directions": {"CLK": "input", "D": "input", "Q": "output"},
                    "connections": {"CLK": [2], "D": [3, 4], "Q": [5, 6]}}},
        "netnames": {"state": {"bits": [5, 6], "attributes": {"init": "x0"}}},
    }


class TransitionGuards(unittest.TestCase):
    def test_total_boolean_encoding_requires_all_used_bits_driven_once(self):
        model = {'ports': {'a': {'direction': 'input', 'bits': [2]},
                           'q': {'direction': 'output', 'bits': [3]}},
                 'cells': {'n': {'type': '$_NOT_', 'connections': {'A': [2], 'Y': [3]},
                                 'port_directions': {'A': 'input', 'Y': 'output'}}}}
        limits = transition.Limits()
        self.assertTrue(transition.total_boolean_reference(model, limits))
        for port, bits in [('A', [4]), ('A', ['x']), ('Y', [2])]:
            bad = copy.deepcopy(model)
            bad['cells']['n']['connections'][port] = bits
            self.assertFalse(transition.total_boolean_reference(bad, limits))
        bad = copy.deepcopy(model)
        bad['cells']['n']['type'] = '$shiftx'
        self.assertFalse(transition.total_boolean_reference(bad, limits))

    def test_storage_alias_inverter_polarity_is_retained(self):
        model = fixture()
        for source, target in [(5, 40), (40, 41)]:
            model['cells'][str(target)] = {'type': '$_NOT_',
                'connections': {'A': [source], 'Y': [target]},
                'port_directions': {'A': 'input', 'Y': 'output'}}
        model['netnames'].update({'inverted': {'bits': [40]}, 'buffered': {'bits': [41]}})
        aliases = transition.state_output_aliases(model, {5, 6}, transition.Limits())
        self.assertEqual(aliases['inverted'], (5, 1))
        self.assertEqual(aliases['buffered'], (5, 0))

    def test_reference_aliases_only_group_consistent_state_names(self):
        groups = transition.reference_alias_groups(
            {'a': 10, 'b': 11, 'c': 12, 'd': 12},
            {'a': 50, 'b': 50, 'c': 60, 'd': 61}, {10, 11, 12}, 20)
        self.assertEqual(groups, {50: {10, 11}})

    def test_anonymous_generate_aliases_are_only_unique_proposals(self):
        aliases = [{'top.genblk3.named.q[1]': 5}, {'top.named.q[1]': 9}]
        self.assertEqual(transition.anonymous_scope_pairs(aliases, [{5}, {9}], 10),
                         [('top.named.q[1]', 5, 9)])
        aliases[0]['top.genblk4.named.q[1]'] = 6
        self.assertEqual(transition.anonymous_scope_pairs(aliases, [{5, 6}, {9}], 10), [])
        self.assertEqual(transition.anonymous_scope_pairs(
            [{'top.genblk3[0].q': 5}, {'top.q': 9}], [{5}, {9}], 10), [])

    def test_fresh_ids_include_unconnected_ports_and_names(self):
        model = fixture()
        model['ports']['unused'] = {'direction': 'input', 'bits': [1000]}
        model['netnames']['unconnected'] = {'bits': [2000, 'x']}
        self.assertEqual(transition.next_bit_id(model), 2001)

    def test_every_storage_bit_is_retained_before_projection(self):
        states, aliases, initial = transition.info(fixture(), transition.Limits())
        self.assertEqual(set(states), {5, 6})
        self.assertEqual(aliases, {"state[0]": 5, "state[1]": 6})
        self.assertEqual(initial, {5: "0"})

    def test_storage_and_work_budgets_fail_closed(self):
        with self.assertRaises(ValueError):
            transition.Limits(max_cells=0)
        with self.assertRaises(ValueError):
            transition.info(fixture(), transition.Limits(max_state=1))
        bad = fixture()
        bad["cells"]["storage"]["connections"]["D"] = [3]
        with self.assertRaises(ValueError):
            transition.info(bad, transition.Limits())

    def test_primary_inputs_cannot_also_be_state_outputs(self):
        bad = fixture()
        bad["ports"]["data"]["bits"] = [5, 4]
        with self.assertRaises(ValueError):
            transition.info(bad, transition.Limits())

    def test_source_assumptions_cannot_enter_the_proof(self):
        bad = fixture()
        bad["cells"]["storage"]["type"] = "$assume"
        with self.assertRaises(ValueError):
            transition.info(bad, transition.Limits())

    def test_undefined_event_controls_are_rejected(self):
        model = {"ports": {"__lec_control_0_clock": {"direction": "output", "bits": ["x"]}},
                 "cells": {}}
        with self.assertRaises(ValueError):
            transition.guard_controls(model, transition.Limits())

    def test_gate_level_flipflop_interface_is_explicit(self):
        model = fixture()
        cell = model["cells"]["storage"]
        cell["type"] = "$_DFF_P_"
        cell["connections"] = {"C": [2], "D": [3], "Q": [5]}
        cell["port_directions"] = {"C": "input", "D": "input", "Q": "output"}
        model["netnames"]["state"]["bits"] = [5]
        model["netnames"]["state"]["attributes"]["init"] = "0"
        states, _, _ = transition.info(model, transition.Limits())
        self.assertEqual(states[5]["clock"], 2)
        self.assertEqual(states[5]["pol"], 1)
        cell["type"] = "$_DFF_NP1_"
        cell["connections"]["R"] = [7]
        cell["port_directions"]["R"] = "input"
        states, _, _ = transition.info(model, transition.Limits())
        self.assertEqual(states[5]["type"], "$adff")
        self.assertEqual((states[5]["pol"], states[5]["reset_pol"], states[5]["value"]), (0, 1, "1"))

    def test_transition_report_requires_every_certificate_guard(self):
        guards = ["equivalent", "initial_relation_covers_all_gate_initial_states",
                  "all_state_mapped_or_universally_quantified", "clock_reset_cones_defined",
                  "implementation_function_total", "all_output_bits_compared", "port_order_verified",
                  "projection_graphs_unchanged", "reference_unknowns_encoded", "refinement_masks_checked"]
        certificate = {key: True for key in guards}
        certificate.update({key: "a" * 64 for key in ["gold_aiger_sha256", "gate_aiger_sha256",
                                                    "cec_log_sha256", "state_relation_sha256", "reference_xprop_sha256"]})
        certificate["outputs"] = 10
        certificate["outputs_proven"] = 10
        certificate["cec_partitioning"] = False
        certificate["reference_case_semantics"] = {"policy": "procedural-priority-no-case-promises", "source_semantics_complete": True}
        certificate["reference_case_normalization_sha256"] = "b" * 64
        certificate['asynchronous_state_visibility'] = 'reset value while reset is active; stored state otherwise arbitrary'
        proof = {"method": transition.METHOD, "status": "ok", "verdict": "proven",
                 "partitions": 10, "certificate": certificate}
        self.assertTrue(report.complete_external_proof(proof))
        for key in guards:
            bad = copy.deepcopy(proof)
            del bad["certificate"][key]
            self.assertFalse(report.complete_external_proof(bad), key)
        dual = copy.deepcopy(proof)
        dual['method'] = transition.DUAL_METHOD
        del dual['certificate']['implementation_function_total']
        extra = ('implementation_unknowns_encoded', 'implementation_definedness_checked',
                 'definedness_refinement_checked')
        dual['certificate'].update({key: True for key in extra})
        dual['certificate']['implementation_xprop_sha256'] = 'c' * 64
        self.assertTrue(report.complete_external_proof(dual))
        for key in (*extra, 'implementation_xprop_sha256'):
            bad = copy.deepcopy(dual)
            del bad['certificate'][key]
            self.assertFalse(report.complete_external_proof(bad), key)

    def test_aiger_symbols_are_checked_in_actual_port_order(self):
        payload = b"aig 3 2 0 1 1\n6\n" + bytes([2, 2]) + b"i0 a\ni1 b\no0 y\nc\ncontrol\n"
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "model.aig"
            path.write_bytes(payload)
            result = transition.aig_interface(path, transition.Limits())
            self.assertEqual(result["inputs"], ["a", "b"])
            self.assertEqual(result["outputs"], ["y"])
            path.write_bytes(payload.replace(b"i1 b", b"i0 b"))
            with self.assertRaises(ValueError):
                transition.aig_interface(path, transition.Limits())

    def test_output_projection_preserves_graph_and_polarity(self):
        payload = b"aig 3 2 0 2 1\n6\n3\n" + bytes([2, 2]) + b"i0 a\ni1 b\no0 and_out\no1 inverted_a\nc\n"
        with tempfile.TemporaryDirectory() as tmp:
            source, target = Path(tmp) / "source.aig", Path(tmp) / "projection.aig"
            source.write_bytes(payload)
            image = transition.aig_interface(source, transition.Limits(), include_graph=True)
            transition.project_outputs(image, 1, 1, target, transition.Limits())
            actual = transition.aig_interface(target, transition.Limits(), include_graph=True)
            self.assertEqual(actual["inputs"], image["inputs"])
            self.assertEqual(actual["graph"], image["graph"])
            self.assertEqual(actual["literals"], [3])
            self.assertEqual(actual["outputs"], ["inverted_a"])

    def test_cec_verdict_is_explicit_and_unambiguous(self):
        self.assertTrue(transition.cec_equivalent("Networks are equivalent.\n"))
        self.assertTrue(transition.cec_equivalent("Networks are equivalent after structural hashing. Time = 0\n"))
        self.assertFalse(transition.cec_equivalent("Networks are NOT EQUIVALENT.\n"))
        self.assertFalse(transition.cec_equivalent("UNDECIDED\n"))
        self.assertFalse(transition.cec_equivalent("Networks are equivalent.\nVerification failed\n"))
