#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Campaign guard regressions, not Vivado validation.

Memory estimate: one Python test worker plus one tiny Python child, <128 MiB;
fixtures <64 KiB, no EDA graph, PDK parsing, path enumeration or build jobs.
"""
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest

import run


class CampaignGuards(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.campaign = run.Campaign.__new__(run.Campaign)
        self.campaign.out = self.directory
        self.campaign.result = {"stages": {}}
        self.campaign.scope = False
        self.campaign.lock = self.directory / "guard.lock"
        self.campaign.args = SimpleNamespace(timeout=2, lock_timeout=1)

    def command(self, source):
        return [sys.executable, "-c", source]

    def test_only_a_normal_counterexample_exit_passes_negative_control(self):
        self.campaign.run("counterexample", self.command(
            "print('RUSTSYS_MISMATCH', flush=True); raise SystemExit(1)"),
            negative=True, marker="RUSTSYS_PASS")
        self.assertEqual(self.campaign.result["stages"]["counterexample"]["status"], "passed")
        for label, source in [
            ("compile-failure", "raise SystemExit(1)"),
            ("abnormal-exit", "print('RUSTSYS_MISMATCH'); raise SystemExit(2)"),
            ("contradictory-pass", "print('RUSTSYS_MISMATCH RUSTSYS_PASS'); raise SystemExit(1)"),
        ]:
            with self.subTest(label=label), self.assertRaises(RuntimeError):
                self.campaign.run(label, self.command(source), negative=True, marker="RUSTSYS_PASS")
            self.assertEqual(self.campaign.result["stages"][label]["status"], "failed")

    def test_timeout_after_mismatch_is_not_a_counterexample(self):
        self.campaign.args.timeout = 0.1
        with self.assertRaises(RuntimeError):
            self.campaign.run("timeout", self.command(
                "import time; print('RUSTSYS_MISMATCH', flush=True); time.sleep(10)"),
                negative=True, marker="RUSTSYS_PASS")
        receipt = self.campaign.result["stages"]["timeout"]
        self.assertEqual(receipt["returncode"], 124)
        self.assertEqual(receipt["status"], "failed")
        self.assertIsNotNone(receipt["maximum_rss_kib"])

    def test_annotation_and_timing_diagnostics_override_a_pass_marker(self):
        for label, warning in [
            ("annotation", "WARNING: SDF annotation failed for dut"),
            ("setuphold", "Warning: Timing violation in $setuphold"),
            ("recovery", "Timing constraint violation in $recrem"),
        ]:
            with self.subTest(label=label), self.assertRaises(RuntimeError):
                self.campaign.run(label, self.command(f"print({warning!r}); print('RUSTSYS_PASS')"),
                    marker="RUSTSYS_PASS", timing_checks=True)
            receipt = self.campaign.result["stages"][label]
            self.assertEqual(receipt["status"], "failed")
            self.assertIn(warning, receipt["timing_diagnostic"])
        self.campaign.run("clean", self.command("print('RUSTSYS_PASS')"),
            marker="RUSTSYS_PASS", timing_checks=True)
        self.assertEqual(self.campaign.result["stages"]["clean"]["status"], "passed")

    def test_zero_diagnostic_summaries_allow_a_successful_simulation(self):
        summaries = [
            "INFO: SDF annotation completed with 0 warnings and 0 errors",
            "INFO: 0 timing violations",
            "INFO: no timing constraint violations were detected",
            "INFO: SDF annotation completed with no warnings or errors",
            "INFO: SDF annotation completed without warnings and errors",
            "INFO: SDF summary: warnings=0, errors=0; timing violations: 0",
        ]
        for summary in summaries:
            with self.subTest(summary=summary):
                self.assertFalse(run.timing_diagnostic(summary))
        source = f"print({chr(10).join(summaries)!r}); print('RUSTSYS_PASS')"
        self.campaign.run("zero-summary", self.command(source), marker="RUSTSYS_PASS", timing_checks=True)
        self.assertEqual(self.campaign.result["stages"]["zero-summary"]["status"], "passed")

    def test_real_diagnostics_remain_failures_even_beside_zero_counts(self):
        diagnostics = [
            "WARNING: SDF annotation completed with 0 errors, but 1 warning",
            "SDF annotation summary: 0 warnings and 2 errors",
            "INFO: SDF summary: warnings=1, errors=0",
            "INFO: SDF annotation failed with 0 reported errors",
            "Warning: $setuphold(posedge C, posedge D, 1.0, 0.2) at /ibex_tb/dut/reg",
            "** Error: SDF $hold check could not be annotated for /ibex_tb/dut/reg",
            "# ** Warning: $recovery(posedge CLR, posedge C, 0.1) failed",
            "Timing constraint violation in $recrem; 0 SDF annotation errors",
            "INFO: 1 timing violation, 0 SDF warnings",
        ]
        for diagnostic in diagnostics:
            with self.subTest(diagnostic=diagnostic):
                self.assertTrue(run.timing_diagnostic(diagnostic))

    def test_routed_sdf_accepts_both_vendor_net_delay_encodings(self):
        sdf = self.directory / "timing.sdf"
        cell = "(CELL (CELLTYPE \"LUT1\") (INSTANCE lut) (DELAY (ABSOLUTE (IOPATH I0 O (1:2:3)))))"
        for kind, arc in [
            ("port", "(PORT I0 (0.1:0.2:0.3))"),
            ("interconnect", "(INTERCONNECT src/O lut/I0 (0.1:0.2:0.3))"),
        ]:
            with self.subTest(kind=kind):
                sdf.write_text(f"(DELAYFILE {cell} (CELL (DELAY (ABSOLUTE {arc}))))")
                arcs = run.sdf_arcs(sdf, routed=True)
                self.assertEqual(arcs["iopath"], 1)
                self.assertEqual(arcs[kind], 1)
        sdf.write_text(f"(DELAYFILE {cell})")
        self.assertEqual(run.sdf_arcs(sdf, routed=False)["iopath"], 1)
        with self.assertRaisesRegex(ValueError, "interconnect timing arcs"):
            run.sdf_arcs(sdf, routed=True)
        sdf.write_text("(DELAYFILE (CELL (DELAY (ABSOLUTE (PORT I0 (1:2:3))))))")
        with self.assertRaisesRegex(ValueError, "cell/interconnect"):
            run.sdf_arcs(sdf, routed=True)

    def test_timing_plan_uses_timing_library_and_explicit_dut_annotation(self):
        c = self.campaign
        c.inputs = self.directory
        c.xvlog, c.xelab, c.xsim_bin, c.glbl = "xvlog", "xelab", "xsim", "glbl.v"
        netlist = self.directory / "post_implementation_timing.v"
        netlist.with_name("post_implementation.sdf").write_text(
            "(DELAYFILE (CELL (DELAY (ABSOLUTE (IOPATH I O (1:2:3)) (PORT I (1:2:3))))))")
        calls = []
        c.run = lambda name, argv, **kwargs: calls.append((name, argv, kwargs))
        c.xsim("xsim-post-implementation-timing", netlist, self.directory / "reference.v", timing=True)
        _, argv, options = calls[1]
        libraries = [argv[i + 1] for i, value in enumerate(argv[:-1]) if value == "-L"]
        self.assertEqual(libraries, ["simprims_ver", "secureip"])
        self.assertEqual(argv[argv.index("-sdfmax") + 1],
                         f"/ibex_tb/dut={self.directory / 'post_implementation.sdf'}")
        for flag in ["-nospecify", "-notimingchecks", "-nosdfinterconnectdelays", "-sdfnoerror", "-sdfnowarn"]:
            self.assertNotIn(flag, argv)
        self.assertTrue(options["timing_checks"])
        self.assertTrue(all(options["timing_checks"] for _, _, options in calls[1:]))


if __name__ == "__main__":
    unittest.main()
