# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
from pathlib import Path
import tempfile
import unittest

import equiv
import lec_trace


class CounterexampleTests(unittest.TestCase):
    def test_nested_names_partial_ports_and_x_mask(self):
        trace = """$scope module miter $end
$scope module __po_cpu $end
$var wire 2 a data<0>__gold $end
$var wire 2 b data<0>__gate $end
$var wire 2 c data<0>__gold_x $end
$upscope $end
$var wire 2 d __po_bus__3_2__gold $end
$var wire 2 e __po_bus__3_2__gate $end
$var wire 2 f __po_bus__3_2__gold_x $end
$upscope $end
#0
b00 a
b10 b
b10 c
b00 d
b01 e
b00 f
#10
b00 c
"""
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "trace.vcd"
            path.write_text(trace)
            self.assertEqual(lec_trace.differences(path, {"cpu.data[0]", "bus"}),
                             {"cpu.data[0]", "bus"})
            with self.assertRaises(equiv.AdaptationError):
                lec_trace.differences(path, {"bus"})
            with self.assertRaises(equiv.AdaptationError):
                lec_trace.differences(path, {"cpu.data[0]", "bus"}, max_work=1)
            with self.assertRaises(equiv.AdaptationError):
                lec_trace.differences(path, {"cpu.data[0]", "bus"}, max_bytes=1)


if __name__ == "__main__":
    unittest.main()
