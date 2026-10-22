#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Read published JSON without extracting a historical evidence archive.

Baseline reports are below 8 MiB decoded. Limit compressed and decoded payloads
to 16 MiB each, index files to 64 KiB, and ordered parts to 128. One reader is
used at a time; expected RSS is below 256 MiB, without graph or solver state.
"""
from pathlib import Path
import gzip
import hashlib
import io
import json


def _bytes(path, limit):
    if path.stat().st_size > limit:
        raise ValueError(f"oversized archived report: {path}")
    with path.open('rb') as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ValueError(f"oversized archived report: {path}")
    return data


def read(path, *, max_bytes=16 * 1024**2, max_parts=128):
    path = Path(path)
    if max_bytes < 1 or max_parts < 1:
        raise ValueError('archive budgets must be positive')
    if path.is_file():
        return json.loads(_bytes(path, max_bytes))
    compressed = path.with_name(path.name + '.gz')
    if compressed.is_file():
        payload = _bytes(compressed, max_bytes)
    else:
        index = compressed.with_name(compressed.name + '.index.json')
        record = json.loads(_bytes(index, 64 * 1024))
        parts = record.get('parts')
        if (record.get('original_name') != compressed.name
                or not isinstance(parts, list) or not 1 <= len(parts) <= max_parts):
            raise ValueError('invalid archive index or part budget')
        chunks = []
        names = set()
        size = 0
        for name in parts:
            if (not isinstance(name, str) or Path(name).name != name
                    or name in ('', '.', '..') or name in names):
                raise ValueError('invalid or repeated archive part name')
            names.add(name)
            chunk = _bytes(index.parent / name, max_bytes - size)
            size += len(chunk)
            chunks.append(chunk)
        payload = b''.join(chunks)
        if hashlib.sha256(payload).hexdigest() != record.get('sha256'):
            raise ValueError('archive combined SHA-256 mismatch')
    with gzip.GzipFile(fileobj=io.BytesIO(payload)) as stream:
        decoded = stream.read(max_bytes + 1)
    if len(decoded) > max_bytes:
        raise ValueError('decoded archive exceeds the report budget')
    return json.loads(decoded)
