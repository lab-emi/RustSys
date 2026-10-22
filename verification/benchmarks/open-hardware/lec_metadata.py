#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Bounded, short-lived JSON analysis; release its heap before EQY starts.

At most one million wire bits per side and 256 MiB per JSON file; under a
4 GiB address-space cap. No cell semantics or reference implementation is read.
"""
import argparse
import fnmatch
import json
from pathlib import Path

import equiv
import lec
import lec_canonical
import run


def main(args):
    if args.canonical_cuts:
        args.state_only = True
        args.port_aliases_only = False
    for path in (args.gold, args.gate):
        if path.stat().st_size > lec.MAX_METADATA_BYTES:
            raise equiv.AdaptationError("metadata byte budget reached")
    gold = json.loads(args.gold.read_text())
    gate = json.loads(args.gate.read_text())
    preferred = (set(gold["modules"][args.top]["ports"]) if args.canonical_cuts
                 else run.read_json(args.register_hints) if args.register_hints else ())
    if len(preferred) > lec.MAX_METADATA_BITS:
        raise equiv.AdaptationError("register hint count budget reached")
    bound = lec.correspondence_bindings(gold, gate, args.top, internal_aliases=not args.port_aliases_only,
                                       preferred=preferred)
    excluded = set()
    if args.exclude_internal or args.state_only:
        names, ports = lec.metadata_names(gold, args.top)
        excluded = {name for name in names.keys() - ports
                    if any(name == pattern or fnmatch.fnmatchcase(name, pattern)
                           for pattern in args.exclude_internal)}
        if args.state_only:
            state = set()
            for name, wire in gold["modules"][args.top]["netnames"].items():
                marker = wire.get("attributes", {}).get("lec_state", "0")
                marked = int(marker, 2) if isinstance(marker, str) else marker
                if marked:
                    if name in ports or len(wire["bits"]) == 1:
                        state.add(name)
                    else:
                        state.update(f"{name}[{index}]" for index in range(len(wire["bits"])))
            selected = {name for name in names.keys() - ports
                        if any(name == pattern or fnmatch.fnmatchcase(name, pattern)
                               for pattern in args.state_wire)}
            excluded |= names.keys() - ports - state - selected
    groups = []
    if args.group_bits:
        names, ports = lec.metadata_names(gold, args.top)
        other, _ = lec.metadata_names(gate, args.top, allowed=names)
        groups = lec.word_groups((names.keys() & other.keys()) - ports, args.group_bits)
    canonical = {}
    if args.canonical_cuts:
        names, ports = lec.metadata_names(gold, args.top)
        other, _ = lec.metadata_names(gate, args.top, allowed=names)
        canonical["canonical_matches"] = sorted((names.keys() & other.keys()) - excluded - (bound - ports))
        canonical["combinational_outputs"] = sorted(
            name for name, port in gold["modules"][args.top]["ports"].items()
            if port["direction"] in ("output", "inout") and name not in state)
        canonical["output_alias_groups"] = lec_canonical.output_alias_groups(
            (gold, gate), args.top,
            {n: p["direction"] for n, p in gold["modules"][args.top]["ports"].items()})
    run.save(args.output, {**canonical, "candidates": sorted(bound), "configured_exclusions": sorted(excluded),
                           "word_groups": groups,
                           "ports": {name: port["direction"] for name, port in
                                     gold["modules"][args.top]["ports"].items()}})


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gold", type=Path, required=True)
    parser.add_argument("--gate", type=Path, required=True)
    parser.add_argument("--top", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--exclude-internal", action="append", default=[])
    parser.add_argument("--state-wire", action="append", default=[])
    parser.add_argument("--group-bits", type=int, choices=(0, 8, 16, 32, 64), default=0)
    parser.add_argument("--state-only", action="store_true")
    parser.add_argument("--port-aliases-only", action="store_true")
    parser.add_argument("--register-hints", type=Path)
    parser.add_argument("--canonical-cuts", action="store_true")
    main(parser.parse_args())
