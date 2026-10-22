# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Publication guards. Peak memory estimate: <64 MiB, one test worker."""
import copy
import json
from pathlib import Path
import tempfile
import unittest
import report
import run


def sample():
    return {"status": "ok", "wall_s": 1.0, "peak_rss_kib": 1024,
            "qor": {"area_um2": 20, "cells": 5}, "netlist_sha256": "abc"}


def external_proof(verdict="proven"):
    return {"status": "ok", "verdict": verdict, "method": "eqy-safe-replacement-v2", "partitions": 4}


class PublicationTests(unittest.TestCase):
    def test_failure_is_not_a_measured_zero(self):
        row = {"design": "Example", "library": "sky130hd_tt", "rustsys": {"status": "failed"},
               "yosys": {"status": "ok", "median_wall_s": 2.5, "peak_rss_kib": 1024,
                         "qor": {"area_um2": 20}}}
        self.assertIn("— / 2.50", report.table({"rows": [row]}))
        self.assertEqual(report.speedups({"rows": [row]}), [])

    def test_unknown_or_internal_only_proof_does_not_support_a_speedup(self):
        tool = {"status": "ok", "median_wall_s": 1, "equivalence": external_proof("unverified"),
                "independent_checker": {"status": "ok", "verdict": "equivalent"}}
        row = {"rustsys": tool, "yosys": copy.deepcopy(tool)}
        self.assertFalse(report.proven(row))
        self.assertEqual(report.speedups({"rows": [row]}), [])
        row["rustsys"]["equivalence"]["verdict"] = "proven"
        self.assertFalse(report.proven(row))
        row["yosys"]["equivalence"]["verdict"] = "proven"
        self.assertTrue(report.proven(row))

    def test_failed_or_missing_proof_status_cannot_be_published_as_proven(self):
        tool = {"status": "ok", "median_wall_s": 1,
                "equivalence": external_proof(),
                "independent_checker": {"status": "ok", "verdict": "equivalent"}}
        for status in (None, "timeout", "failed", "unverified"):
            row = {"rustsys": copy.deepcopy(tool), "yosys": copy.deepcopy(tool)}
            if status is None:
                row["yosys"]["equivalence"].pop("status")
            else:
                row["yosys"]["equivalence"]["status"] = status
            self.assertFalse(report.proven(row))
            self.assertEqual(report.speedups({"rows": [row]}), [])

    def test_headers_and_proof_results_identify_each_tool(self):
        base = {"status": "ok", "median_wall_s": 1, "peak_rss_kib": 1024,
                "qor": {"area_um2": 20}, "equivalence": external_proof()}
        row = {"design": "Example", "library": "sky130hd_tt", "rustsys": copy.deepcopy(base),
               "yosys": copy.deepcopy(base)}
        row["yosys"]["equivalence"] = {"status": "timeout", "verdict": "unverified"}
        table = report.table({"rows": [row]})
        self.assertEqual(table.splitlines()[0].count("RustSys / Yosys"), 4)
        self.assertIn("| Proven / Unverified |", table)

    def test_conditional_or_empty_proofs_cannot_be_labeled_proven(self):
        proof = external_proof()
        self.assertTrue(report.complete_external_proof(proof))
        for count in (0, None, True, {}):
            self.assertFalse(report.complete_external_proof({**proof, "partitions": count}))
        for method in ("", "rtl-refinement-bit-positions-v3", "yosys-equiv-induct"):
            self.assertFalse(report.complete_external_proof({**proof, "method": method}))

    def test_changed_qor_or_netlist_invalidates_aggregate(self):
        a, b = sample(), sample()
        b["netlist_sha256"] = "def"
        self.assertEqual(run.summarize([a, b])["status"], "nondeterministic")
        b = sample()
        b["qor"]["cells"] = 6
        self.assertEqual(run.summarize([a, b])["status"], "nondeterministic")

    def test_failed_repeat_cannot_be_hidden_by_successful_median(self):
        a, b = sample(), sample()
        b["status"] = "timeout"
        self.assertEqual(run.summarize([a, b])["status"], "unverified")

    def test_aggregate_uses_median_time_and_worst_peak_memory(self):
        a, b, c = sample(), sample(), sample()
        a["wall_s"], b["wall_s"], c["wall_s"] = 2, 1, 9
        b["peak_rss_kib"] = 3000
        r = run.summarize([a, b, c])
        self.assertEqual(r["median_wall_s"], 2)
        self.assertEqual(r["peak_rss_kib"], 3000)

    def test_only_scope_metadata_is_excluded_from_yosys_gate_counts(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            path = directory / "report.json"
            data = {"design": {"num_cells": 9, "area": 42,
                               "num_cells_by_type": {"$scopeinfo": 4, "nand": 5}}}
            path.write_text(json.dumps(data))
            q = run.qos(directory, "yosys")
            self.assertEqual(q["cells"], 5)
            self.assertEqual(q["unmapped"], {})
            data["design"]["num_cells_by_type"]["$_AND_"] = 1
            path.write_text(json.dumps(data))
            self.assertEqual(run.qos(directory, "yosys")["unmapped"], {"$_AND_": 1})


if __name__ == "__main__":
    unittest.main()
