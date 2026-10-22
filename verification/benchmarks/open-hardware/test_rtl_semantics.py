# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
import hashlib
from pathlib import Path
import tempfile
import unittest

import rtl_semantics


class CasePromises(unittest.TestCase):
    def test_verilog_promises_are_removed_before_parsing(self):
        original = (b'module t;\n'
                    b'// ordinary full_case text\n'
                    b'wire \\full_case(*x*) ;\n'
                    b'initial $display("(* full_case *)");\n'
                    b'always @(*) begin\n'
                    b'(* full_case, keep = "a,b*)", parallel_case *) case(a)\n'
                    b'0:q=1;endcase end\nendmodule\n')
        with tempfile.TemporaryDirectory() as directory:
            src, dst = (Path(directory) / name for name in ("source.v", "priority.v"))
            src.write_bytes(original)
            result = rtl_semantics.verilog_priority(src, dst)
            actual = dst.read_bytes()
            self.assertEqual(len(actual), len(original))
            self.assertEqual(actual.count(b'\n'), original.count(b'\n'))
            self.assertIn(b'keep = "a,b*)"', actual)
            self.assertIn(b'always @(*)', actual)
            self.assertIn(b'wire \\full_case(*x*) ;', actual)
            self.assertIn(b'$display("(* full_case *)")', actual)
            self.assertEqual(result['removed'], {'full_case': 1, 'parallel_case': 1})

    def test_empty_attributes_and_comment_pragmas_preserve_source_positions(self):
        source = b'(*full_case*) case(s)\n// synopsys parallel_case\n// full_case text\n'
        with tempfile.TemporaryDirectory() as directory:
            src, dst = (Path(directory) / name for name in ("source.v", "priority.v"))
            src.write_bytes(source)
            result = rtl_semantics.verilog_priority(src, dst)
            self.assertEqual(dst.read_bytes(), b'              case(s)\n// synopsys              \n// full_case text\n')
            self.assertEqual(result['removed'], {'full_case': 1, 'parallel_case': 1})

    def test_only_exact_attribute_records_change(self):
        original = (b'  attribute \\full_case 1\n'
                    b'  attribute \\src "full_case and parallel_case"\n'
                    b'  attribute \\parallel_case 1\n'
                    b'  attribute \\full_case_suffix 1\n'
                    b'  switch \\full_case\n'
                    b'    case 1\'1\n')
        expected = b''.join(original.splitlines(keepends=True)[i] for i in (1, 3, 4, 5))
        with tempfile.TemporaryDirectory() as directory:
            src, dst = (Path(directory) / name for name in ("source.il", "priority.il"))
            src.write_bytes(original)
            result = rtl_semantics.priority_cases(src, dst)
            self.assertEqual(dst.read_bytes(), expected)
            self.assertEqual(src.read_bytes(), original)
            self.assertEqual(result['removed'], {'full_case': 1, 'parallel_case': 1})
            self.assertEqual(result['removed_lines'], [1, 3])
            self.assertEqual(result['source_sha256'], hashlib.sha256(original).hexdigest())
            self.assertEqual(result['normalized_sha256'], hashlib.sha256(expected).hexdigest())

    def test_overwrite_and_same_path_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            src, dst = (Path(directory) / name for name in ("source.il", "priority.il"))
            src.write_bytes(b'module \\top\nend\n')
            with self.assertRaises(ValueError):
                rtl_semantics.priority_cases(src, src)
            dst.write_bytes(b'existing')
            with self.assertRaises(FileExistsError):
                rtl_semantics.priority_cases(src, dst)

    def test_streaming_case_filter_enforces_its_explicit_budget(self):
        with tempfile.TemporaryDirectory() as directory:
            src, dst = (Path(directory) / name for name in ('source.il', 'priority.il'))
            src.write_bytes(b'module \\top\nend\n')
            with self.assertRaises(ValueError):
                rtl_semantics.priority_cases(src, dst, max_bytes=4)
            with self.assertRaises(ValueError):
                rtl_semantics.priority_cases(src, dst, max_bytes=1024**3 + 1)
            self.assertFalse(dst.exists())


if __name__ == '__main__':
    unittest.main()
