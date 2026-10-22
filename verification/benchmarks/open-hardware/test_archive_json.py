#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Archive transport and budget controls; <2 MiB data, no EDA worker."""
from pathlib import Path
import gzip
import hashlib
import json
import tempfile
import unittest

import archive_json


class ArchiveJson(unittest.TestCase):
    def test_plain_gzip_and_split_preserve_the_same_record(self):
        expected = {'source_sha256': 'a' * 64, 'rows': [{'verdict': 'proven'}]}
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'results.json'
            raw = json.dumps(expected).encode()
            path.write_bytes(raw)
            self.assertEqual(archive_json.read(path), expected)
            path.unlink()
            payload = gzip.compress(raw, mtime=0)
            compressed = path.with_suffix('.json.gz')
            compressed.write_bytes(payload)
            self.assertEqual(archive_json.read(path), expected)
            compressed.unlink()
            parts = []
            for i in range(0, len(payload), 16):
                part = f'part-{i // 16}'
                (path.parent / part).write_bytes(payload[i:i + 16])
                parts.append(part)
            index = path.with_suffix('.json.gz.index.json')
            record = {'original_name': compressed.name, 'parts': parts,
                      'sha256': hashlib.sha256(payload).hexdigest()}
            index.write_text(json.dumps(record))
            self.assertEqual(archive_json.read(path), expected)
            (path.parent / parts[-1]).write_bytes(b'corrupt')
            with self.assertRaisesRegex(ValueError, 'SHA-256'):
                archive_json.read(path)
            record['parts'] = ['../outside']
            index.write_text(json.dumps(record))
            with self.assertRaisesRegex(ValueError, 'part name'):
                archive_json.read(path)

    def test_compression_cannot_bypass_the_decoded_budget(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'results.json'
            path.with_suffix('.json.gz').write_bytes(gzip.compress(b'"' + b'a' * 1000 + b'"'))
            with self.assertRaisesRegex(ValueError, 'decoded archive'):
                archive_json.read(path, max_bytes=128)


if __name__ == '__main__':
    unittest.main()
