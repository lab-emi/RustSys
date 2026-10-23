# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Optional-library integrity controls: one tiny ZIP, <64 MiB, no EDA runs."""
import hashlib
import io
from pathlib import Path
import tempfile
import unittest

import fetch_additional_pdks
import run
import yosys_initial


class AdditionalPdkTests(unittest.TestCase):
    def test_download_requires_both_archive_and_member_bindings(self):
        buffer=io.BytesIO();data=b'library(example) {}\n'
        import zipfile
        with zipfile.ZipFile(buffer,'w') as archive:archive.writestr('lib/example.lib',data)
        raw=buffer.getvalue()
        spec={'archive_bytes':len(raw),'archive_sha256':hashlib.sha256(raw).hexdigest(),
              'size':len(data),'sha256':hashlib.sha256(data).hexdigest(),'member':'lib/example.lib'}
        self.assertEqual(fetch_additional_pdks.checked_member(raw,spec),data)
        for key,value in [('archive_sha256','0'*64),('sha256','0'*64),('size',len(data)+1)]:
            with self.assertRaises(ValueError):fetch_additional_pdks.checked_member(raw,{**spec,key:value})
        with self.assertRaises(ValueError):fetch_additional_pdks.checked_member(raw+b'changed',spec)

    def test_unknown_library_cannot_silently_use_another_pdks_latches(self):
        cfg={'library':'new.lib','design':{'top':'top','files':['input.v']}}
        with tempfile.TemporaryDirectory() as name:
            with self.assertRaisesRegex(ValueError,'explicit latch map'):
                yosys_initial.script(cfg,Path(name))
            cfg['latch_map']=str(run.LEC_FIXTURES/'latches-ez130.v')
            script=yosys_initial.script(cfg,Path(name))
            self.assertIn('latches-ez130.v',script)
            self.assertNotIn('latches-gf180.v',script)
            self.assertIn('abc -liberty',script)


if __name__=='__main__':unittest.main()
