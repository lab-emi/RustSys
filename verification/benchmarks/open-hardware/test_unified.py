# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Unified table's completeness/binding controls.40 small records, <64MiB, one worker."""
import copy
import unittest

import scaling_report
import scaling_verify


def matrix():
    data={'scope':{'required_reference_cores':[1,16]},'rows':[]}
    proofs={'rows':[]};timing={'rows':[]}
    for did in scaling_report.ORDER:
        for lib in scaling_report.LIBS:
            p={'id':did,'library':lib};s=copy.deepcopy(p)
            for cores in (1,16):
                row={'id':did,'design':did,'library':lib,'allocated_physical_cores':cores}
                for tool in ('rustsys','yosys'):
                    digest=did+lib+tool
                    row[tool]={'status':'ok','samples':[{'status':'ok','netlist_sha256':digest} for _ in range(3)],
                        'median_wall_s':8.0 if cores==1 else 2.0,'median_average_cpu_cores':3.5,
                        'peak_rss_kib':1024,'qor':{'area_um2':20},'netlist_sha256':digest}
                    p[tool]={'method':'eqy-safe-replacement-v2','status':'ok','verdict':'proven','partitions':4,
                             'source_sha256':{'netlist':digest}}
                    s[tool]={'status':'ok','fmax_mhz':100.0,'sha256':{'netlist':digest}}
                data['rows'].append(row)
            proofs['rows'].append(p);timing['rows'].append(s)
    return data,proofs,timing


class UnifiedTests(unittest.TestCase):
    def test_both_tools_and_both_allocations_share_one_table(self):
        data,p,s=matrix()
        main,secondary,_,_=scaling_report.tables(data,p,s)
        self.assertIn('| 1 core, s',main)
        self.assertIn('| 16 cores, s',main)
        self.assertEqual(main.count('| RustSys |'),10)
        self.assertEqual(main.count('| Yosys |'),10)
        self.assertEqual(secondary,'')

    def test_missing_yosys_one_core_cannot_pass_as_complete(self):
        data,p,s=matrix()
        data['rows'][0].pop('yosys')
        self.assertFalse(scaling_verify.ready(data))
        with self.assertRaises(ValueError):scaling_report.tables(data,p,s)

    def test_different_thread_output_needs_its_own_proof(self):
        data,p,s=matrix()
        data['rows'][0]['yosys']['netlist_sha256']='changed'
        with self.assertRaises(ValueError):scaling_report.tables(data,p,s)

    def test_unverified_proof_and_failed_repeat_cannot_be_published(self):
        data,p,s=matrix();p['rows'][0]['rustsys']['verdict']='unverified'
        with self.assertRaises(ValueError):scaling_report.tables(data,p,s)
        data,p,s=matrix();data['rows'][0]['yosys']['samples'][1]['status']='timeout'
        with self.assertRaises(ValueError):scaling_report.tables(data,p,s)


if __name__=='__main__':unittest.main()
