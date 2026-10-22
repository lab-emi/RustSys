# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Resource-guard regression: two tiny Python processes, <64 MiB total expected."""
from pathlib import Path
import sys
import tempfile
import time
import unittest

import run


class ResourceGuardTests(unittest.TestCase):
    def test_live_budget_failure_stops_the_child_and_is_never_a_pass(self):
        with tempfile.TemporaryDirectory() as name:
            def exhausted():
                raise ValueError("fixture output budget reached")
            result = run.execute([sys.executable, "-c", "import time; time.sleep(5)"],
                                 Path(name), "budget", seconds=10, memory_gib=1,
                                 monitor=exhausted)
            self.assertEqual(result["status"], "resource_limit")
            self.assertEqual(result["exit_code"], 125)
            self.assertEqual(result["diagnostic"]["code"], "BENCH_RESOURCE_LIMIT")

    def test_timeout_collects_peak_rss_and_terminates_descendants(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            marker = directory / "surviving-child"
            child = f"import time; from pathlib import Path; time.sleep(0.4); Path({str(marker)!r}).write_text('alive')"
            parent = f"import subprocess,sys,time; subprocess.Popen([sys.executable, '-c', {child!r}]); time.sleep(5)"
            result = run.execute([sys.executable, "-c", parent], directory, "timeout",
                                 seconds=0.1, memory_gib=1)
            self.assertEqual(result["status"], "timeout")
            self.assertEqual(result["exit_code"], 124)
            self.assertGreater(result["peak_rss_kib"], 0)
            self.assertGreater(result["wall_s"], 0)
            time.sleep(0.45)
            self.assertFalse(marker.exists(), "a descendant escaped the process-group deadline")


if __name__ == "__main__":
    unittest.main()
