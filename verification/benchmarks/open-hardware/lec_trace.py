# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Bounded inspection of external EQY counterexamples (never a proof oracle)."""
import re

import equiv


def differences(path, expected_outputs, max_bytes=128 * 1024**2,
                max_signals=200_000, max_steps=10_000, max_work=10_000_000):
    """Find defined gold/gate output differences, including nested scopes/slices.

    This only guides removal of optional INTERNAL match points. Every actual
    equivalence verdict must still come from complete external certificates.
    """
    if path.stat().st_size > max_bytes:
        raise equiv.AdaptationError("counterexample byte budget reached")
    names, values, scope, found = {}, {}, [], set()
    steps = work = 0

    def sample():
        nonlocal work
        for name, value in values.items():
            work += 1
            if work > max_work:
                raise equiv.AdaptationError("counterexample work budget reached")
            if not name.startswith("__po_") or not name.endswith("__gold"):
                continue
            stem = name[:-6]
            gate, mask = values.get(stem + "__gate"), values.get(stem + "__gold_x", "0")
            if gate is None:
                continue
            try:
                mismatch = (int(value, 2) ^ int(gate, 2)) & ~int(mask, 2)
            except ValueError as error:
                raise equiv.AdaptationError("non-Boolean counterexample value") from error
            if not mismatch:
                continue
            point = stem[5:].replace("<", "[").replace(">", "]")
            if point not in expected_outputs:
                sliced = re.fullmatch(r"(.*)__[0-9]+_[0-9]+", point)
                if sliced and sliced[1] in expected_outputs:
                    point = sliced[1]
                else:
                    raise equiv.AdaptationError("unknown counterexample output: " + point)
            found.add(point)

    with path.open() as stream:
        for line in stream:
            if line.startswith("$scope"):
                scope.append(line.split()[2])
                if len(scope) > 256:
                    raise equiv.AdaptationError("counterexample scope depth budget reached")
            elif line.startswith("$upscope"):
                if not scope:
                    raise equiv.AdaptationError("unbalanced counterexample scope")
                scope.pop()
            elif line.startswith("$var"):
                parts = line.split()
                name = ".".join(scope[1:] + [parts[4]])
                if name.startswith("__po_"):
                    names.setdefault(parts[3], []).append(name)
                if len(names) > max_signals:
                    raise equiv.AdaptationError("counterexample signal budget reached")
            elif line.startswith("#"):
                sample()
                steps += 1
                if steps > max_steps:
                    raise equiv.AdaptationError("counterexample step budget reached")
            elif line.startswith("b"):
                value, identifier = line[1:].split()
                for name in names.get(identifier, ()):
                    values[name] = value
    sample()
    return found
