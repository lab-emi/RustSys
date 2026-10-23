# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Optional campaign publication guards: five records, <64 MiB, no EDA runs."""
import copy
import unittest

import additional_report
import scaling_report
from test_unified import matrix


def additional():
    data,proofs,timing=matrix()
    data['rows']=[row for row in data['rows'] if row['allocated_physical_cores']==16 and row['library']==scaling_report.LIBS[0]]
    data['limits']={'repetitions':3,'warmups':1}
    proofs['rows']=[row for row in proofs['rows'] if row['library']==scaling_report.LIBS[0]]
    timing['rows']=[row for row in timing['rows'] if row['library']==scaling_report.LIBS[0]]
    for row in data['rows']:
        row['library']=additional_report.LIBRARY;row['cpu_affinity']=list(range(16))
    for collection,field in ((proofs,'source_sha256'),(timing,'sha256')):
        for row in collection['rows']:
            row['library']=additional_report.LIBRARY
            for tool in ('rustsys','yosys'):row[tool][field]['library']='verified-lib'
    return data,proofs,timing,{'sha256':'verified-lib'}


class AdditionalReportTests(unittest.TestCase):
    def test_every_design_and_both_tools_are_required(self):
        records=additional()
        self.assertEqual(set(additional_report.new_rows(*records)),set(scaling_report.ORDER))
        for index in (0,1,2):
            changed=copy.deepcopy(records);changed[index]['rows'].pop()
            with self.assertRaises(ValueError):additional_report.new_rows(*changed)

    def test_changed_pdk_or_incomplete_proof_cannot_publish(self):
        for field,value in (('verdict','unverified'),('source_sha256',{'library':'other'})):
            records=additional();records[1]['rows'][0]['yosys'][field]=value
            with self.assertRaises(ValueError):additional_report.new_rows(*records)
        records=additional();records[2]['rows'][0]['rustsys']['sha256']['library']='other'
        with self.assertRaises(ValueError):additional_report.new_rows(*records)

    def test_nonidentical_repetition_or_missing_memory_is_rejected(self):
        records=additional();records[0]['rows'][0]['rustsys']['samples'][0]['netlist_sha256']='changed'
        with self.assertRaises(ValueError):additional_report.new_rows(*records)
        records=additional();records[0]['rows'][0]['yosys']['peak_rss_kib']=None
        with self.assertRaises(ValueError):additional_report.new_rows(*records)


if __name__=='__main__':unittest.main()
