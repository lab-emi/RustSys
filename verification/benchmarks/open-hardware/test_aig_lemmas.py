# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Small transport guards, <32MiB. Actual reconstruction uses external ABC."""
from array import array
from pathlib import Path
import re
import tempfile
import unittest

import aig_lemmas as a


class AigLemmaTests(unittest.TestCase):
    def test_word_expression_matches_integer_bits_including_aliases(self):
        patterns=([2,4,6],[3,4,0],[2,2,1],[0,1,5])
        for bits in patterns:
            for signed in (False,True):
                text=a.word_spec(bits,signed)
                for mask in range(8):
                    def replace(m):
                        width,offset=map(int,m.groups());return str((mask>>offset)&((1<<width)-1))
                    got=eval(re.sub(r'\[(\d+):(\d+)\]',replace,text),{'__builtins__':{}})
                    values=[(mask>>(x//2-1)&1)^(x&1) if x>=2 else x for x in bits]
                    want=sum(value*(1<<k) for k,value in enumerate(values))
                    if signed and values[-1]:want-=1<<len(bits)
                    self.assertEqual(got,want)

    def test_component_rejects_unobserved_free_input(self):
        graph=a.Graph(2,array('I',[4,2]),array('I',[6]))
        with self.assertRaisesRegex(ValueError,'outside'):
            a.component(graph,{'a':[2],'b':[0],'c':[],'y':[6]})

    def test_context_refuses_replacing_original_inputs_and_aliases(self):
        graph=a.Graph(2,array('I',[4,2]),array('I',[6]))
        for result in ([2],[6,6]):
            with self.assertRaises(ValueError):a.abstract_context(graph,[{'a':[2],'b':[4],'c':[],'y':result}])

    def test_binary_roundtrip_and_topology_guard(self):
        graph=a.Graph(2,array('I',[4,2]),array('I',[6,7]))
        with tempfile.TemporaryDirectory() as folder:
            path=Path(folder)/'x.aig';graph.write(path);got=a.Graph.read(path)
            self.assertEqual((got.inputs,got.ands,got.outputs),(graph.inputs,graph.ands,graph.outputs))
            bad=a.Graph(2,array('I',[8,2]),array('I',[6]))
            with self.assertRaises(ValueError):bad.write(Path(folder)/'bad.aig')


if __name__=='__main__':unittest.main()
