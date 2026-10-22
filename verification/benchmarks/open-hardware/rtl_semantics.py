# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Remove optional case synthesis promises from external frontend output.

The reference is procedural Verilog, including priority when several case items
match.  full_case/parallel_case are synthesis promises, not reset or input
constraints.  Keeping them in the oracle can silently change that reference.
Only two attribute records are removed; all RTLIL logic/process records remain
byte-for-byte intact.  The original and normalized artifacts are both retained.
"""
import hashlib
from pathlib import Path
import re

MAX_BYTES = 256 * 1024**2
MAX_LINE = 1024**2
MAX_REMOVED = 100_000
ATTRIBUTE = re.compile(rb"\s*attribute \\(full_case|parallel_case)\s+[^\r\n]+\r?\n?\Z")
HINT = re.compile(rb"(?:full_case|parallel_case)\b")


def _blank(data):
    return bytes(c if c in (10, 13) else 32 for c in data)


def _comment_end(data, pos):
    if data[pos:pos + 2] == b'//':
        end = data.find(b'\n', pos + 2)
        return len(data) if end < 0 else end
    end = data.find(b'*/', pos + 2)
    if end < 0:
        raise ValueError('unterminated Verilog comment')
    return end + 2


def _string_end(data, pos):
    pos += 1
    while pos < len(data):
        if data[pos] == 92:
            pos += 2
        elif data[pos] == 34:
            return pos + 1
        else:
            pos += 1
    raise ValueError('unterminated Verilog string')


def verilog_priority(source, destination):
    """Strip promises before the frontend can use them; preserve byte positions.

    This is a bounded lexical attribute edit, not an RTL interpreter. Strings,
    normal comments, and other attributes remain intact. Prepared benchmark
    sources are self-contained, as required by the benchmark manifest.
    """
    source, destination = Path(source), Path(destination)
    if source.stat().st_size > MAX_BYTES or source.resolve() == destination.resolve():
        raise ValueError('invalid case-normalization source or byte budget')
    data = source.read_bytes()
    output = bytearray(data)
    counts = {'full_case': 0, 'parallel_case': 0}
    pos = 0
    while pos < len(data):
        token = data[pos:pos + 2]
        if token in (b'//', b'/*'):
            end = _comment_end(data, pos)
            body = data[pos + 2:end]
            if re.match(rb'\s*(?:synopsys|synthesis|pragma)\b', body):
                for match in HINT.finditer(body):
                    counts[match[0].decode()] += 1
                    a, b = pos + 2 + match.start(), pos + 2 + match.end()
                    output[a:b] = _blank(data[a:b])
            pos = end
            continue
        if data[pos] == 34:
            pos = _string_end(data, pos)
            continue
        if data[pos] == 92:
            while pos < len(data) and not chr(data[pos]).isspace():
                pos += 1
            continue
        # @(*) is an event expression, not an attribute.
        if token != b'(*' or data[pos + 2:pos + 3] == b')':
            pos += 1
            continue
        start = pos
        cursor, depth, commas = pos + 2, 0, []
        while cursor < len(data):
            if cursor - start > MAX_LINE:
                raise ValueError('Verilog attribute size budget reached')
            if data[cursor:cursor + 2] in (b'//', b'/*'):
                cursor = _comment_end(data, cursor)
            elif data[cursor] == 34:
                cursor = _string_end(data, cursor)
            elif data[cursor] == 92:
                while cursor < len(data) and not chr(data[cursor]).isspace():
                    cursor += 1
            elif data[cursor:cursor + 2] == b'*)' and depth == 0:
                break
            else:
                c = data[cursor]
                if c in b'([{':
                    depth += 1
                elif c in b')]}':
                    depth -= 1
                elif c == 44 and depth == 0:
                    commas.append(cursor)
                if not 0 <= depth <= 128:
                    raise ValueError('Verilog attribute nesting budget reached')
                cursor += 1
        else:
            raise ValueError('unterminated Verilog attribute')
        edges = [start + 1, *commas, cursor]
        segments = [(edges[i] + 1, edges[i + 1]) for i in range(len(edges) - 1)]
        kept, removed = [], False
        for index, (a, b) in enumerate(segments):
            match = re.match(rb'\s*(full_case|parallel_case)\b', data[a:b])
            if match:
                counts[match[1].decode()] += 1
                removed = True
            else:
                kept.append(index)
        if removed:
            end = cursor + 2
            output[start:end] = _blank(data[start:end])
            if kept:
                output[start:start + 2], output[cursor:end] = b'(*', b'*)'
                for index in kept:
                    a, b = segments[index]
                    output[a:b] = data[a:b]
                for index in kept[:-1]:
                    output[commas[index]] = 44
        pos = cursor + 2
        if sum(counts.values()) > MAX_REMOVED:
            raise ValueError('Verilog case-promise budget reached')
    with destination.open('xb') as stream:
        stream.write(output)
    return {'source': str(source.resolve()), 'normalized': str(destination.resolve()),
            'source_sha256': hashlib.sha256(data).hexdigest(),
            'normalized_sha256': hashlib.sha256(output).hexdigest(),
            'removed': counts, 'bytes': len(data)}


def contains_case_promises(files):
    """Conservative fast path for the manifest's self-contained prepared RTL."""
    used = 0
    found = False
    for name in files:
        path = Path(name)
        used += path.stat().st_size
        if used > MAX_BYTES:
            raise ValueError("case-scan source budget reached")
        data = path.read_bytes()
        found |= b"full_case" in data or b"parallel_case" in data
    return found


def priority_cases(source, destination, max_bytes=MAX_BYTES):
    if type(max_bytes) is not int or not 0 < max_bytes <= 1024**3:
        raise ValueError('case-normalization byte budget must be in 1..1 GiB')
    source, destination = Path(source), Path(destination)
    if source.resolve() == destination.resolve():
        raise ValueError("case normalization requires a separate output artifact")
    if source.stat().st_size > max_bytes:
        raise ValueError("case-normalization byte budget reached")
    old_hash, new_hash = hashlib.sha256(), hashlib.sha256()
    removed = []
    counts = {"full_case": 0, "parallel_case": 0}
    consumed = 0
    with source.open("rb") as src, destination.open("xb") as dst:
        number = 0
        while line := src.readline(MAX_LINE + 1):
            number += 1
            consumed += len(line)
            if len(line) > MAX_LINE or consumed > max_bytes:
                raise ValueError("case-normalization input budget reached")
            old_hash.update(line)
            match = ATTRIBUTE.fullmatch(line)
            if match:
                if len(removed) == MAX_REMOVED:
                    raise ValueError("case-normalization attribute budget reached")
                removed.append(number)
                counts[match[1].decode()] += 1
                continue
            dst.write(line)
            new_hash.update(line)
    return {"policy": "procedural-priority-no-case-promises", "removed": counts,
            "removed_lines": removed, "source_sha256": old_hash.hexdigest(),
            "normalized_sha256": new_hash.hexdigest(), "source_bytes": consumed}
