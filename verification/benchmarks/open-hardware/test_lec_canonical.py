# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
import tempfile
import unittest
from pathlib import Path

import equiv
import lec_canonical


class CanonicalWireTests(unittest.TestCase):
    def test_marker_preserves_connections_and_initial_values(self):
        text = "module \\top\n  attribute \\init 1'1\n  wire \\state\n  wire output 1 \\q\n  connect \\q \\state\nend\n"
        with tempfile.TemporaryDirectory() as tmp:
            source, output = Path(tmp) / "in.il", Path(tmp) / "out.il"
            source.write_text(text)
            lec_canonical.annotate(source, output, {"q"})
            self.assertEqual(output.read_text().replace("  attribute \\lec_canonical 1\n", ""), text)
            lec_canonical.verify_names(output, {"q", "state"})
            with self.assertRaises(equiv.AdaptationError):
                lec_canonical.annotate(source, output, {"missing"})
            with self.assertRaises(equiv.AdaptationError):
                lec_canonical.annotate(source, output, {"q"}, max_bytes=3)

    def test_output_aliases_are_grouped_across_both_sides(self):
        def model(a, b, c):
            return {"modules": {"top": {"ports": {
                "a": {"bits": [a]}, "b": {"bits": [b]}, "c": {"bits": [c]},
                "constant": {"bits": ["0"]}}}}}
        ports = dict.fromkeys(("a", "b", "c", "constant"), "output")
        models = (model(1, 1, 2), model(3, 4, 4))
        self.assertEqual(lec_canonical.output_alias_groups(models, "top", ports), [["a", "b", "c"]])
        with self.assertRaises(equiv.AdaptationError):
            lec_canonical.output_alias_groups(models, "top", ports, max_group=2)
