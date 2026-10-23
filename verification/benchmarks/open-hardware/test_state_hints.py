# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Small public-metadata parser controls; <64 KiB, no model or solver jobs."""
from pathlib import Path
import tempfile
import unittest

import ez130_state_hints as hints


def ff(instance,q,cell='DFFRQX3'):
    return f'  {cell} {instance} (\n    .CK(clk),\n    .D(d),\n    .Q({q}),\n    .RB(rn)\n  );\n'


class StateHintTests(unittest.TestCase):
    def test_scalar_alias_and_bounded_pre_only_state(self):
        with tempfile.TemporaryDirectory() as folder:
            pre,post=Path(folder)/'pre.v',Path(folder)/'post.v'
            pre.write_text(ff('_1_','anchor')+ff('_2_','_70_')+ff('_3_','unused')+ff('_4_','last')
                           +'  assign \\core.q  = ~_70_;\n')
            post.write_text(ff('_10_','anchor')+ff('_11_','_99_')+ff('_12_','last'))
            alignment=hints.align(pre,post)
            self.assertEqual(alignment['anchors'],2)
            self.assertEqual(len(alignment['pre_only']),1)
            self.assertTrue(alignment['not_a_proof'])
            self.assertEqual(hints.propose(pre,alignment),
                             [{'rtl_state':'core.q','gate_instance':'_11_','inverted':True}])

    def test_ambiguous_alias_is_rejected(self):
        with tempfile.TemporaryDirectory() as folder:
            pre,post=Path(folder)/'pre.v',Path(folder)/'post.v'
            pre.write_text(ff('_1_','anchor')+ff('_2_','_70_')+'  assign q = ~_70_;\n  assign other = _70_;\n')
            post.write_text(ff('_10_','anchor')+ff('_11_','_99_'))
            with self.assertRaises(ValueError):hints.propose(pre,hints.align(pre,post))

    def test_unsupported_state_or_unbounded_line_is_rejected(self):
        with tempfile.TemporaryDirectory() as folder:
            pre,post=Path(folder)/'pre.v',Path(folder)/'post.v'
            pre.write_text(ff('_1_','anchor'))
            post.write_text(ff('_10_','anchor',cell='LATQX1'))
            with self.assertRaises(ValueError):hints.align(pre,post)
            pre.write_text('x'*65537+'\n')
            with self.assertRaises(ValueError):list(hints.lines(pre))


if __name__=='__main__':unittest.main()
