# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Bounded name adaptation for an external Yosys proof.

SYNTH-REQ-0603 names packed state by bit position, omitting the suffix at width
one. Convert only public wire names to that convention; Yosys still proves every
matched state/output function. Structural memory lowering preserves RTL undefined
choices, checked as refinement with -undef. The native X/Z=0 policy has separate
fully specified positive/negative fixtures in validate_equiv.py. No truth functions
or reference-tool source are read.
"""
from pathlib import Path
import re
import time

MAX_BYTES = 128 * 1024 * 1024
MAX_DECLARATIONS = 2_000_000
MAX_VECTOR_BITS = 1_000_000
METHOD = "rtl-refinement-bit-positions-v3"
OBJECT = re.compile(r"[A-Za-z_][A-Za-z0-9_$\.\[\]:-]*")
DECL = re.compile(
    r"^\s*(input|output|inout|wire|reg)\s+"
    r"(?:(?:wire|reg)\s+)?(?:signed\s+)?"
    r"(?:\[\s*(-?\d+)\s*:\s*(-?\d+)\s*\]\s+)?"
    r"(\\\S+|[A-Za-z_$][A-Za-z0-9_$]*)\s*(?:=[^;]*)?;\s*$"
)


class AdaptationError(ValueError):
    pass


def quote(value):
    return '"' + str(value).replace("\\", "\\\\").replace('"', '\\"') + '"'


def object_name(value):
    # Yosys file arguments accept quotes, but object-name arguments retain them.
    # Decline names requiring command-language escaping rather than guess or inject text.
    if not OBJECT.fullmatch(value):
        raise AdaptationError("unsupported object name: " + value[:160])
    return value


def source_limits(files, max_bytes=MAX_BYTES, max_files=4096):
    """Bound input work without interpreting or rewriting the source language."""
    if len(files) > max_files:
        raise AdaptationError("source-file budget reached")
    if sum(Path(name).stat().st_size for name in files) > max_bytes:
        raise AdaptationError("source-byte budget reached")


def declaration_lines(text, top=None, deadline=None):
    """Yield module-scope lines, excluding helper-function/task local declarations."""
    has_modules = re.search(r"(?m)^\s*module\s", text) is not None
    active, seen, subprogram = not has_modules, not has_modules, None
    selected = top
    for number, line in enumerate(text.splitlines()):
        if deadline is not None and number % 1024 == 0 and time.monotonic() >= deadline:
            raise AdaptationError("wire adaptation deadline reached")
        module = re.match(r"\s*module\s+(\\\S+|[A-Za-z_$][A-Za-z0-9_$]*)", line)
        if module:
            name = module[1].removeprefix("\\")
            selected = name if selected is None else selected
            active = name == selected
            seen |= active
        if not active:
            continue
        start = re.match(r"\s*(function|task)\b", line)
        if start:
            if subprogram:
                raise AdaptationError("nested helper subprogram is unsupported")
            subprogram = start[1]
            if re.search(r"\bend" + subprogram + r"\b", line):
                subprogram = None
            continue
        if subprogram:
            if re.match(r"\s*end" + subprogram + r"\b", line):
                subprogram = None
            continue
        yield line
        if re.match(r"\s*endmodule\b", line):
            active = False
    if not seen or subprogram:
        raise AdaptationError("missing top module or incomplete helper subprogram")


def renames(text, top, max_declarations=MAX_DECLARATIONS, max_bits=MAX_VECTOR_BITS, deadline=None,
            include_ports=False, zero_scalar_only=False):
    """Return collision-checked renames applied after splitnets; never add logic."""
    if len(text.encode()) > MAX_BYTES:
        raise AdaptationError("lowered Verilog exceeds the byte budget")
    names, ports, buses = set(), set(), {}
    for number, line in enumerate(declaration_lines(text, top, deadline)):
        if deadline is not None and number % 1024 == 0 and time.monotonic() >= deadline:
            raise AdaptationError("wire adaptation deadline reached")
        m = DECL.fullmatch(line)
        if not m:
            if re.match(r"^\s*(input|output|inout|wire|reg)\b", line):
                raise AdaptationError("unsupported lowered declaration: " + line[:160])
            continue
        kind, left, right, name = m.groups()
        name = name.removeprefix("\\")
        names.add(name)
        if len(names) > max_declarations:
            raise AdaptationError("wire declaration budget reached")
        if kind in ("input", "output", "inout"):
            ports.add(name)
        if left is not None and not name.startswith("$") and (kind not in ("input", "output", "inout") or include_ports):
            bounds = int(left), int(right)
            if name in buses and buses[name] != bounds:
                raise AdaptationError("conflicting ranges for " + name)
            buses[name] = bounds
    buses = {name: bounds for name, bounds in buses.items() if include_ports or name not in ports}
    bits = sum(abs(left - right) + 1 for left, right in buses.values())
    if bits > max_bits:
        raise AdaptationError("vector-bit adaptation budget reached")
    # Check literal identifiers against split-bit names without materializing all
    # bits of already canonical [width-1:0] vectors.
    for name in names:
        suffix = re.fullmatch(r"(.*)\[(-?\d+)\]", name)
        if suffix and suffix[1] in buses:
            left, right = buses[suffix[1]]
            if min(left, right) <= int(suffix[2]) <= max(left, right):
                raise AdaptationError("split bit collides with a literal identifier: " + name)
    changing = {name: (left, right) for name, (left, right) in buses.items()
                if (left == right == 0 if zero_scalar_only else
                    left == right or right != 0 or left < right)}
    # A huge vector with a long identifier must be declined before constructing one name
    # per bit: a small input header can otherwise expand into terabytes of rename text.
    expanded = sum((abs(left - right) + 1) * (4 * len(name.encode()) + 192)
                   for name, (left, right) in changing.items())
    if expanded > MAX_BYTES:
        raise AdaptationError("expanded bit-name budget reached")
    pairs = []
    for name, (left, right) in sorted(changing.items()):
        if deadline is not None and time.monotonic() >= deadline:
            raise AdaptationError("wire adaptation deadline reached")
        width = abs(left - right) + 1
        for offset in range(width):
            index = right + offset if left >= right else right - offset
            old = f"{name}[{index}]"
            new = name if width == 1 else f"{name}[{offset}]"
            if old in names:
                raise AdaptationError("split bit collides with a literal identifier: " + old)
            pairs.append((old, new))
    old_names = {old for old, _ in pairs}
    targets = {new for _, new in pairs}
    if len(targets) != len(pairs) or targets & (names - buses.keys()):
        raise AdaptationError("canonical bit names collide with existing wires")
    # Two passes also handle overlapping old/new index ranges such as [4:2] -> [2:0].
    prefix = "rustsys_proof_bit_"
    for _ in range(16):
        if not any(n.startswith(prefix) for n in names | old_names | targets):
            break
        prefix = "_" + prefix
    else:
        raise AdaptationError("cannot allocate a temporary wire-name namespace")
    # Most RTL uses [width-1:0], already matching the bit-position contract.
    # Do not run two Yosys rename passes for tens of thousands of identity moves.
    moves = [(old, new) for old, new in pairs if old != new]
    commands = [f"cd {object_name(top)}"]
    commands += [f"rename {object_name(old)} {prefix}{i}" for i, (old, _) in enumerate(moves)]
    commands += [f"rename {prefix}{i} {object_name(new)}" for i, (_, new) in enumerate(moves)]
    commands.append("cd ..")
    result = "\n".join(commands) + "\n"
    if len(result.encode()) > MAX_BYTES:
        raise AdaptationError("rename script exceeds the byte budget")
    return result


def lower_script(config, directory):
    """Write the common, structural-only external Verilog lowering script."""
    directory = Path(directory)
    top, lib = config["design"]["top"], config["library"]
    gold, gate = directory / "equiv-rtl.v", directory / "equiv-gates.v"
    lower = directory / "equiv-lower.ys"
    rtl_read = "read_verilog -sv " + ("-icells " if config.get('rtl_icells') else "")
    gate_read = "read_verilog " + ("-icells " if config.get('gate_icells') else "")
    reference_library = (f"read_liberty -ignore_miss_func -ignore_miss_dir {quote(lib)}\n"
                         if config.get('reference_gate_netlist', False) else "")
    lower.write_text(
        reference_library + rtl_read + " ".join(map(quote, config["design"]["files"])) + "\n"
        f"hierarchy -check -top {top}\nproc\nflatten\nmemory_collect\nmemory_map\nopt_clean\nasync2sync\n"
        f"write_verilog -noattr -norename {quote(gold)}\ndesign -reset\n"
        f"read_liberty -ignore_miss_func -ignore_miss_dir {quote(lib)}\n"
        + gate_read + f"{quote(config.get('netlist', directory / 'netlist.v'))}\n"
        f"hierarchy -check -top {top}\nproc\nflatten\nmemory_collect\nmemory_map\nopt_clean\nasync2sync\n"
        f"write_verilog -noattr -norename {quote(gate)}\n"
    )

    return lower, gold, gate


def prove(config, directory, yosys, execute, seconds=60, memory_gib=4):
    """Lower, adapt names, then prove; one deadline covers all phases."""
    directory = Path(directory)
    start = time.monotonic()
    steps = []
    top, lib = config["design"]["top"], config["library"]
    if not OBJECT.fullmatch(top):
        return {"status": "unverified", "verdict": "unverified", "method": METHOD,
                "wall_s": None, "peak_rss_kib": None, "steps": [],
                "diagnostic": {"code": "BENCH_EQUIV_ADAPTATION", "message": "unsupported top name"}}
    lower, gold, gate = lower_script(config, directory)

    def done(status, verdict="unverified", diagnostic=None, diagnostic_code="BENCH_EQUIV_ADAPTATION"):
        peaks = [s["peak_rss_kib"] for s in steps if s.get("peak_rss_kib") is not None]
        result = {"status": status, "verdict": verdict, "method": METHOD,
                  "scope": {"rtl_undefined_values": "refinement via equiv_simple/equiv_induct -undef",
                            "golden_lowering": "structural memory_map; no memory_dff or opt don't-care choices",
                            "native_xz_policy": "separately checked per SYNTH-REQ-0201",
                            "initial_state": "unchanged; no setundef or new reset/initial-state assumptions"},
                  "wall_s": time.monotonic() - start,
                  "peak_rss_kib": max(peaks) if peaks else None, "steps": steps}
        if diagnostic:
            result["diagnostic"] = {"code": diagnostic_code, "message": diagnostic}
        return result

    def step(script, name):
        remaining = seconds - (time.monotonic() - start)
        if remaining <= 0:
            return {"status": "timeout", "peak_rss_kib": None}
        r = execute([yosys, "-Q", "-T", "-s", str(script)], directory, name,
                    seconds=remaining, memory_gib=memory_gib)
        steps.append(r)
        return r

    try:
        source_limits(config["design"]["files"])
    except (OSError, UnicodeError, AdaptationError) as error:
        return done("unverified", diagnostic=str(error))
    first = step(lower, "equiv-lower")
    if first["status"] != "ok":
        return done(first["status"])
    try:
        for path in (gold, gate):
            if path.stat().st_size > MAX_BYTES:
                raise AdaptationError("lowered netlist exceeds the file budget")
        gold_names = renames(gold.read_text(), top, deadline=start + seconds)
        gate_names = renames(gate.read_text(), top, deadline=start + seconds)
    except (OSError, AdaptationError) as error:
        return done("unverified", diagnostic=str(error))
    proof = directory / "equiv-canonical.ys"
    proof.write_text(
        f"read_verilog {quote(gold)}\nproc\nsplitnets\n" + gold_names
        + f"rename {top} gold\ndesign -stash gold\n"
        + f"read_verilog {quote(gate)}\nproc\nsplitnets\n" + gate_names
        + f"rename {top} gate\ndesign -copy-from gold gold\n"
        + "equiv_make gold gate equiv\nhierarchy -check -top equiv\n"
        + "equiv_simple -undef -short -seq 1\nequiv_induct -undef -seq 4\nequiv_status -assert\n"
    )
    last = step(proof, "equiv-canonical")
    log = directory / "equiv-canonical.stdout"
    text = log.read_text() if log.exists() else ""
    obligations = re.search(r"Found (\d+) \$equiv cells", text)
    proven = (last["status"] == "ok" and obligations and int(obligations[1]) > 0
              and "Equivalence successfully proven!" in text)
    return done(last["status"], "proven" if proven else "unverified")
