# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Structural-reader adapter guards, <32MiB; actual OpenSTA replay is separate."""
from pathlib import Path
import tempfile
import unittest
import sta_structural


class StructuralStaTests(unittest.TestCase):
    def test_unused_constant_integer_keeps_every_other_byte(self):
        text=b'module m(input a,output y);\n wire signed [31:0] \\unused.loop.i ;\n assign \\unused.loop.i = 32\x27d3;\n assign y=a;\nendmodule\n'
        with tempfile.TemporaryDirectory() as folder:
            src=Path(folder)/'src.v';dst=Path(folder)/'dst.v';src.write_bytes(text)
            path,receipt=sta_structural.adapt(src,dst)
            self.assertEqual(path.read_bytes(),text.replace(b'wire signed',b'wire       '))
            self.assertTrue(receipt['functional_uses_checked_absent'])

    def test_functional_signed_uses_fail_closed(self):
        for use in ('assign wide = n;', 'BUF g(.A(n),.Y(y));','assign n = other;', 'assign y = n[0];'):
            with tempfile.TemporaryDirectory() as folder:
                src=Path(folder)/'src.v';src.write_text('wire signed [31:0] n;\n'+use+'\n')
                with self.assertRaisesRegex(ValueError,'functional use'):
                    sta_structural.adapt(src,Path(folder)/'dst.v')

    def test_unsigned_input_is_returned_without_copying(self):
        with tempfile.TemporaryDirectory() as folder:
            src=Path(folder)/'src.v';dst=Path(folder)/'dst.v';src.write_text('wire [31:0] n;\n')
            self.assertEqual(sta_structural.adapt(src,dst)[0],src)
            self.assertFalse(dst.exists())


if __name__=='__main__':unittest.main()
