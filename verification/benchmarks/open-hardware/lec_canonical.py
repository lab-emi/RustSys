# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Bounded wire-name cleanup for external proof models; never changes cells.

One million name entries at ~300 bytes each plus streamed text (<32 MiB per
line). Metadata is processed separately under a 4 GiB cap. All actual logic
cleanup is performed by the reference executable, not by this adapter.
"""
from pathlib import Path

import equiv

MAX_NAMES = 1_000_000
MAX_MODEL_BYTES = 1024**3
MAX_LINE_BYTES = 32 * 1024**2


def annotate(source, target, names, max_bytes=MAX_MODEL_BYTES):
    """Add only a marker attribute to retained wires in a single RTLIL module."""
    if len(names) > MAX_NAMES or sum(len(n.encode()) for n in names) > 128 * 1024**2:
        raise equiv.AdaptationError("canonical name byte budget reached")
    required = set(names)
    if not required or len(required) > MAX_NAMES:
        raise equiv.AdaptationError("canonical wire count budget reached")
    if Path(source).stat().st_size > max_bytes:
        raise equiv.AdaptationError("canonical model byte budget reached")
    missing = required.copy()
    written = 0
    with Path(source).open("rb") as src, Path(target).open("wb") as dst:
        while line := src.readline(MAX_LINE_BYTES + 1):
            if len(line) > MAX_LINE_BYTES:
                raise equiv.AdaptationError("canonical model line budget reached")
            if line.lstrip().startswith(b"wire "):
                name = line.split()[-1].decode().removeprefix("\\")
                if name in required:
                    marker = b"  attribute \\lec_canonical 1\n"
                    written += len(marker)
                    dst.write(marker)
                    missing.discard(name)
            written += len(line)
            if written > max_bytes:
                raise equiv.AdaptationError("annotated model byte budget reached")
            dst.write(line)
    if missing:
        raise equiv.AdaptationError("canonical wires missing from model: " + str(len(missing)))


def script(source, target, top):
    """Keep chosen names/ports, hide other aliases, and collapse redundant wires."""
    return (f"read_rtlil {equiv.quote(source)}\n"
            f"select -module {equiv.object_name(top)}\n"
            "setattr -unset keep w:* a:lec_canonical %d\n"
            "rename -hide w:* a:lec_canonical %d\n"
            "setattr -set keep 1 a:lec_canonical\n"
            "select -clear\nopt_clean -purge\n"
            f"write_rtlil {equiv.quote(target)}\ndesign -reset\n")


def verify_names(path, names):
    if Path(path).stat().st_size > MAX_MODEL_BYTES:
        raise equiv.AdaptationError("canonical model byte budget reached")
    required = set(names)
    with Path(path).open("rb") as stream:
        while line := stream.readline(MAX_LINE_BYTES + 1):
            if len(line) > MAX_LINE_BYTES:
                raise equiv.AdaptationError("canonical model line budget reached")
            if line.lstrip().startswith(b"wire "):
                required.discard(line.split()[-1].decode().removeprefix("\\"))
    if required:
        raise equiv.AdaptationError("wire cleanup removed a required proof point")


def output_alias_groups(models, top, ports, max_group=4096):
    """Unite literal output aliases only, with linear bounded union/find work."""
    outputs = {n for n, direction in ports.items() if direction in ("output", "inout")}
    if len(outputs) > MAX_NAMES:
        raise equiv.AdaptationError("output alias count budget reached")
    parent = {name: name for name in outputs}

    def find(name):
        # Path halving; every link comes from the bounded output-name set.
        for _ in range(len(parent) + 1):
            if parent[name] == name:
                return name
            parent[name] = parent[parent[name]]
            name = parent[name]
        raise equiv.AdaptationError("cycle in output alias groups")

    for model in models:
        representative = {}
        for name in sorted(outputs):
            bits = model["modules"][top]["ports"][name]["bits"]
            if len(bits) != 1:
                raise equiv.AdaptationError("canonical proof ports must be scalar")
            bit = bits[0]
            if not isinstance(bit, int):
                continue
            previous = representative.setdefault(bit, name)
            a, b = find(previous), find(name)
            if a != b:
                parent[max(a, b)] = min(a, b)
    groups = {}
    for name in sorted(outputs):
        group = groups.setdefault(find(name), [])
        group.append(name)
        if len(group) > max_group:
            raise equiv.AdaptationError("output alias group budget reached")
    return [group for group in groups.values() if len(group) > 1]
