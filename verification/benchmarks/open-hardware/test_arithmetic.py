# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
from pathlib import Path
import tempfile
import unittest

import arithmetic
import report


class ArithmeticEncoding(unittest.TestCase):
    def test_aig_constraints_and_target_are_checked_independently(self):
        with tempfile.TemporaryDirectory() as directory:
            d = Path(directory)
            (d/'a.aag').write_text('aag 3 2 0 2 1\n2\n4\n6\n0\n6 4 2\n')
            (d/'p.polys').write_text('1 4;\n2 -l6+b0*a0;\n3 -s0+l6;\n4 -s1;\n')
            (d/'s.spec').write_text('a0*b0-s0-2*s1;\n')
            arithmetic.check_encoding(d/'a.aag',d/'p.polys',d/'s.spec',1,False)
            (d/'s.spec').write_text('a0*b0-s0-2*s1+1;\n')
            with self.assertRaisesRegex(ValueError, 'target'):
                arithmetic.check_encoding(d/'a.aag',d/'p.polys',d/'s.spec',1,False)
            (d/'s.spec').write_text('a0*b0-s0-2*s1;\n')
            (d/'p.polys').write_text('1 4;\n2 -l6+b0*a0+1;\n3 -s0+l6;\n4 -s1;\n')
            with self.assertRaisesRegex(ValueError, 'constraints'):
                arithmetic.check_encoding(d/'a.aag',d/'p.polys',d/'s.spec',1,False)

    def test_polynomial_comparison_respects_coefficients_and_commutativity(self):
        self.assertEqual(arithmetic.polynomial('2*a*b-b*a+1;'), arithmetic.polynomial('a*b+1;'))
        self.assertNotEqual(arithmetic.polynomial('a*b+1;'), arithmetic.polynomial('a*b;'))
        with self.assertRaises(ValueError):
            arithmetic.polynomial('a/(b+1);')

    def test_composition_needs_both_complete_external_sides(self):
        child = {'method':'eqy-control','status':'ok','verdict':'proven','partitions':1}
        proof = {'method':'arithmetic-inductive-composition-v1','status':'ok','verdict':'proven',
                 'partitions':4,'certificate':{'complete':True,'transitivity_checked':True,
                    'arithmetic_evidence_checked':True,'arithmetic':{'complete':True,'components':[{'width':1}]},'subproofs':[child]}}
        self.assertFalse(report.complete_external_proof(proof))
        proof['certificate']['subproofs'].append(child)
        self.assertTrue(report.complete_external_proof(proof))
        proof['certificate']['arithmetic_evidence_checked'] = False
        self.assertFalse(report.complete_external_proof(proof))


if __name__ == '__main__':
    unittest.main()
