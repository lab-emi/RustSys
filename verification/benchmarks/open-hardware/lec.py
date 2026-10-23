#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Bounded external EQY LEC of measured netlists against their input Verilog.

Reference tools are opaque executables. No RustSys proof model is used. See the
benchmark README for memory estimates; use a whole-run cgroup and scale lock.
"""
import argparse
from datetime import datetime, timezone
import fnmatch
import library_models
import json
import os
from pathlib import Path
import re
import shutil
import sys
import time

import equiv
import lec_canonical
import run
import rtl_semantics

METHOD = "eqy-safe-replacement-v2"
MAX_METADATA_BITS = 1_000_000
MAX_BINDINGS = 100_000
MAX_METADATA_BYTES = 256 * 1024**2
MAX_RULE_BYTES = 128 * 1024**2


def name_pattern(names, suffix="", max_bytes=MAX_RULE_BYTES):
    """Bound command-text expansion before constructing an EQY regexp."""
    if sum(2 * len(name.encode()) + 1 for name in names) + len(suffix) > max_bytes:
        raise equiv.AdaptationError("correspondence rule byte budget reached")
    return "/^(?:" + "|".join(map(re.escape, sorted(names))) + ")" + suffix + "$/"


def metadata_names(model, top, max_bits=MAX_METADATA_BITS, allowed=None):
    """Describe correspondence names, without interpreting any cell function.

    JSON connection vectors are in LSB order. Production snapshots have already
    split ports and applied the producer's naming convention. Unsplitted vectors
    are also accepted for small metadata tests and diagnostics.
    """
    module = model["modules"][top]
    ports = set(module["ports"])
    names, count = {}, 0
    for name, net in module["netnames"].items():
        if "$" in name or not equiv.OBJECT.fullmatch(name):
            continue
        bits = net["bits"]
        count += len(bits)
        if count > max_bits:
            raise equiv.AdaptationError("correspondence metadata bit budget reached")
        if name in ports or len(bits) == 1:
            if allowed is None or name in allowed:
                names[name] = tuple(bits)
        else:
            for index, bit in enumerate(bits):
                key = f"{name}[{index}]"
                if allowed is not None and key not in allowed:
                    continue
                if key in names:
                    raise equiv.AdaptationError("correspondence metadata name collision")
                names[key] = (bit,)
    return names, ports


def register_names(text, top, positions=False):
    """Use actual lowered register declarations ahead of their wire aliases."""
    result = set()
    used = 0
    for line in equiv.declaration_lines(text, top):
        match = equiv.DECL.fullmatch(line)
        if not match or match[1] != "reg":
            continue
        name = match[4].removeprefix("\\")
        if "$" in name or not equiv.OBJECT.fullmatch(name):
            continue
        if match[2] is None:
            used += 1
            if used > MAX_METADATA_BITS:
                raise equiv.AdaptationError("register correspondence budget reached")
            result.add(name)
        else:
            left, right = int(match[2]), int(match[3])
            width = abs(left - right) + 1
            used += width
            if used > MAX_METADATA_BITS:
                raise equiv.AdaptationError("register correspondence budget reached")
            if width == 1 and (positions or left == right == 0):
                result.add(name)
            else:
                for offset in range(width):
                    index = offset if positions else right + offset if left >= right else right - offset
                    result.add(f"{name}[{index}]")
        if len(result) > MAX_METADATA_BITS:
            raise equiv.AdaptationError("register correspondence budget reached")
    return result


def correspondence_bindings(gold, gate, top, max_bits=MAX_METADATA_BITS, internal_aliases=True,
                            preferred=()):
    """Retain constant/alias drivers rather than making inconsistent free cuts.

    Binding does not assert equivalence or remove an output obligation. It tells
    EQY to include the original driving logic in partitions consuming that wire.
    Only connectivity and literal constants from external JSON are consulted.
    """
    left, ports = metadata_names(gold, top, max_bits)
    right, gate_ports = metadata_names(gate, top, max_bits, allowed=left)
    if ports != gate_ports:
        raise equiv.AdaptationError("correspondence metadata ports differ")
    common = left.keys() & right.keys()
    inputs = {name for name, port in gold["modules"][top]["ports"].items()
              if port["direction"] in ("input", "inout")}
    state_names = set()
    for name, wire in gold["modules"][top]["netnames"].items():
        marker = wire.get("attributes", {}).get("lec_state", "0")
        marked = int(marker, 2) if isinstance(marker, str) else marker
        if marked:
            if name in ports or len(wire["bits"]) == 1:
                state_names.add(name)
            else:
                state_names.update(f"{name}[{i}]" for i in range(len(wire["bits"])))
    # Keep true state boundaries ahead of output aliases. Otherwise an output
    # alias can absorb its driving FF and join the entire clock/control loop.
    preferred = set(preferred)
    rank = lambda name: (name not in inputs, name not in preferred,
                         not (name in state_names and name not in ports),
                         name not in ports, len(name), name)
    bound = set()
    for name in sorted(common):
        for a, b in zip(left[name], right[name], strict=True):
            if len(left[name]) == 1 and (a in ("0", "1") or b in ("0", "1")):
                bound.add(name)
    # An optimization may merge state only on one side. Such a correspondence
    # must keep its actual drivers in the proof, rather than allowing the two
    # formerly related values to become independent free inputs.
    for names in (left, right):
        classes = {}
        for name in common:
            for bit in names[name]:
                if isinstance(bit, int):
                    classes.setdefault(bit, []).append(name)
        for members in classes.values():
            canonical = min(members, key=rank)
            bound.update(name for name in members
                         if name != canonical and len(names[name]) == 1
                         and (internal_aliases or name in ports))
    if len(bound) > MAX_BINDINGS:
        raise equiv.AdaptationError("correspondence binding budget reached")
    # Proof models split ports into scalar bits. A duplicate output bit can now
    # retain its driver without binding an unrelated entire output bus. The
    # canonical bit remains a separately proved cutpoint. Primary inputs never
    # acquire assumptions or driver logic.
    bound -= inputs
    return bound


def word_groups(names, width, max_groups=20_000):
    """Bounded buckets of adjacent named bits; no graph traversal or cut search."""
    if width not in (0, 8, 16, 32, 64):
        raise equiv.AdaptationError("unsupported proof group width")
    if not width:
        return []
    groups = {}
    for count, name in enumerate(sorted(names)):
        if count >= MAX_METADATA_BITS:
            raise equiv.AdaptationError("proof group bit budget reached")
        match = re.fullmatch(r"(.*)\[([0-9]+)\]", name)
        if match:
            key = match[1], int(match[2]) // width
            groups.setdefault(key, []).append(name)
            if len(groups) > max_groups:
                raise equiv.AdaptationError("proof group count budget reached")
    return [members for _, members in sorted(groups.items()) if len(members) > 1]


def partition_clock_scripts(work, max_scripts=60_000):
    """Sample clocks after partitioning, when feedback crosses proved cutpoints.

    These are generated proof inputs, not EQY/SBY implementation sources. Keep
    hashes of every change, and reject an unexpected generated script format.
    """
    changed = []
    for path in sorted(work.glob("strategies/*/*/*.sby")):
        if len(changed) >= max_scripts or path.stat().st_size > 1024**2:
            raise equiv.AdaptationError("clock strategy script budget reached")
        text = path.read_text()
        if text.count("\nasync2sync\n") != 1:
            raise equiv.AdaptationError("unsupported generated clock strategy script")
        before = run.digest(path)
        path.write_text(text.replace("\nasync2sync\n", "\nclk2fflogic\nscc -expect 0\n"))
        changed.append({"path": str(path.relative_to(work)), "before_sha256": before,
                        "after_sha256": run.digest(path)})
    if not changed:
        raise equiv.AdaptationError("no generated clock strategy scripts")
    return changed


def output_names(text, top=None):
    names = set()
    for line in equiv.declaration_lines(text, top):
        match = equiv.DECL.fullmatch(line)
        if match and match[1] in ("output", "inout"):
            names.add(equiv.object_name(match[4].removeprefix("\\")))
    if not names:
        raise equiv.AdaptationError("no observable output ports")
    return sorted(names)


def port_shapes(text, top=None):
    ports = {}
    for line in equiv.declaration_lines(text, top):
        match = equiv.DECL.fullmatch(line)
        if match and match[1] in ("input", "output", "inout"):
            left, right = match[2], match[3]
            ports[match[4].removeprefix("\\")] = (
                match[1], abs(int(left) - int(right)) + 1 if left is not None else 1)
    return ports


def port_namespace(texts, top):
    """Keep split proof ports distinct from RustSys's literal bit-wire names."""
    declared = set()
    for text in texts:
        for line in equiv.declaration_lines(text, top):
            match = equiv.DECL.fullmatch(line)
            if match:
                declared.add(match[4].removeprefix("\\"))
    prefix = "__rustsys_lec_port_"
    for _ in range(16):
        if not any(name.startswith(prefix) for name in declared):
            break
        prefix = "_" + prefix
    else:
        raise equiv.AdaptationError("cannot allocate proof port namespace")
    names = sorted(port_shapes(texts[0], top))
    mapping = {name: prefix + str(index) for index, name in enumerate(names)}
    commands = [f"cd {equiv.object_name(top)}"]
    commands += [f"rename {equiv.object_name(name)} {proof}" for name, proof in mapping.items()]
    commands.append("cd ..")
    return mapping, "\n".join(commands) + "\n"


def renamed_port_metadata(text, mapping):
    lines = []
    for line in text.splitlines():
        match = equiv.DECL.fullmatch(line)
        if match:
            name = match[4].removeprefix("\\")
            if name in mapping:
                line = line[:match.start(4)] + mapping[name] + line[match.end(4):]
        lines.append(line)
    return "\n".join(lines) + "\n"


def direct_model_names(text, top, deadline, include_ports=False, zero_scalar_only=False):
    # The Verilog writer creates anonymous concatenation temporaries _0_, _1_, ...
    # that do not exist in the original in-memory model used by EQY. They are not
    # state correspondence points. Keep every declared user/state name unchanged.
    lines = []
    for line in text.splitlines():
        match = equiv.DECL.fullmatch(line)
        if match and (re.fullmatch(r"_[0-9]+_", match[4]) or "$" in match[4]):
            continue
        lines.append(line)
    return equiv.renames("\n".join(lines), top, deadline=deadline, include_ports=include_ports,
                         zero_scalar_only=zero_scalar_only)


def internal_metadata_text(text):
    """Keep port/scalar metadata; memories remain in the functional model.

    An internal-cell snapshot may still declare an unpacked memory. The actual
    frontend structurally lowers it before proof. Its original declaration is
    neither a scalar alias nor a port and must not enter the name-only adapter.
    """
    array = re.compile(r'^\s*(?:wire|reg)\s+(?:signed\s+)?(?:\[[^]]+\]\s+)?'
                       r'(?:\\\S+\s+|[A-Za-z_$][A-Za-z0-9_$]*\s*)'
                       r'(?:\[[^]]+\]\s*)+;\s*$')
    return '\n'.join(line for line in text.splitlines()
                     if not array.fullmatch(line)) + '\n'


def input_alias_outputs(text, top=None):
    """Identify direct port aliases that must not become unrelated cut inputs."""
    inputs, outputs = set(), set(output_names(text, top))
    for line in equiv.declaration_lines(text, top):
        match = equiv.DECL.fullmatch(line)
        if match and match[1] in ("input", "inout"):
            inputs.add(match[4].removeprefix("\\"))
    aliases = set()
    for line in equiv.declaration_lines(text, top):
        match = re.fullmatch(r"\s*assign\s+(\\\S+|[A-Za-z_$][\w$]*)\s*=\s*"
                             r"(\\\S+|[A-Za-z_$][\w$]*)(?:\[\d+(?::\d+)?\])?\s*;\s*", line)
        if match:
            output, source = (name.removeprefix("\\") for name in match.groups())
            if output in outputs and source in inputs:
                aliases.add(output)
    return aliases


def tree_budget(directory, max_bytes=8 * 1024**3, max_files=200000):
    size = count = 0
    for path in directory.rglob("*"):
        if path.is_symlink():
            raise equiv.AdaptationError("unexpected symlink in EQY output")
        if path.is_file():
            size += path.stat().st_size
            count += 1
            if size > max_bytes or count > max_files:
                raise equiv.AdaptationError("EQY output budget reached")


def strategy_results(work, partitions):
    """Read every EQY-generated obligation; missing/unknown status cannot pass."""
    if not partitions or len(set(partitions)) != len(partitions):
        raise equiv.AdaptationError("empty or duplicate EQY partition list")
    results = {}
    base = (work / "strategies").resolve()
    for name in partitions:
        directory = (base / name).resolve()
        if not directory.is_relative_to(base):
            raise equiv.AdaptationError("unsafe EQY partition path")
        statuses = {}
        if directory.exists():
            strategies = list(directory.iterdir())
            if len(strategies) > 64:
                raise equiv.AdaptationError("EQY strategy-count budget reached")
            for strategy in strategies:
                status = strategy / "status"
                if status.is_file() and status.stat().st_size <= 4096:
                    statuses[strategy.name] = status.read_text().strip() or "UNKNOWN"
        # A skipped strategy may contain "PASS (cached)" after a different
        # engine wins. Require an actual calibrated SBY engine to win; native
        # EQY SAT failed a planted output-corruption control in this toolchain.
        supported = {"pdr", "induction", "ric3", "smt_first"}
        results[name] = {"strategies": statuses,
                         "proven": any(engine in supported and status == "PASS"
                                       for engine, status in statuses.items())}
    return results


def compact_functional_names(top, enabled):
    if not enabled:
        return ''
    # Imported RTLIL may contain generated cell IDs from a previous process.
    # Vacate those IDs before simplemap allocates fresh ones. Otherwise an
    # adjacent checkpoint can collide with the new process's auto-ID counter.
    return (f'tee -q rename -enumerate -pattern compact_before_% {top}/c:*\n'
            'simplemap t:$and t:$not\n'
            f'select {top}\n'
            f'setattr -unset src -unset hdlname {top}/c:* {top}/w:*\n'
            f'tee -q rename -enumerate -pattern $proof_% {top}/c:* {top}/w:*$*\n')


def prove(config, directory, yosys, eqy, abc, seconds=10800, memory_gib=8,
          max_partitions=20000, partition_seconds=600, strategy="pdr-smt", bind_outputs=(), bind_wires=(),
          multiclock=False, jobs=1, max_output_gib=8, prune_aliases=False, exclude_internal=(),
          compact_library_wires=False, group_bits=0, state_only=False, amend_outputs=(),
          smt_solver="yices", partition_clocks=False, port_aliases_only=False, canonical_cuts=False,
          isolate_cuts=False, model_only=False, native_storage=False, state_wires=()):
    directory = Path(directory)
    if canonical_cuts:
        state_only, prune_aliases, port_aliases_only = True, True, False
    position_names = config.get('position_names', config.get("tool", "rustsys") != "yosys")
    start = time.monotonic()
    steps, partitions, bound, amended, solvers = [], [], set(), set(), {}
    case_normalization = None
    dual_manifest = None

    def finish(status, proven=False, message=None):
        peaks = [step["peak_rss_kib"] for step in steps if step.get("peak_rss_kib") is not None]
        result = {"method": METHOD, "status": status, "verdict": "proven" if proven else "unverified",
                  "wall_s": time.monotonic() - start, "peak_rss_kib": max(peaks) if peaks else None,
                  "partitions": len(partitions) if partitions else None, "steps": steps,
                  "solver_binaries": solvers,
                  "case_normalization": case_normalization,
                  'library_models': dual_manifest,
                  "limits": {"wall_s": seconds, "memory_gib": memory_gib, "workers": jobs,
                             "frontend_processes": 2, "max_partitions": max_partitions,
                             "partition_wall_s": partition_seconds,
                             "eqy_file_bytes": 1024**3, "output_bytes": max_output_gib * 1024**3,
                             "multiclock": multiclock},
                  "scope": {"relation": "EQY safe-replacement equivalence with formal X propagation",
                            "reference": "input Verilog; structural memory lowering",
                            "case_semantics": "procedural priority; no full_case/parallel_case assumptions",
                            "storage": (("partition-local " if partition_clocks else "")
                                        + "clk2fflogic after lossless Verilog expression normalization; no added reset or initial values"
                                        if multiclock else "sampled-edge async2sync; no added reset or initial values"),
                            "native_literal_zero_policy": "checked separately by RustSys",
                            "outputs": "all top-level outputs; all EQY cutpoint obligations required",
                            "state_names": ("RustSys bit positions" if position_names
                                            else "original Verilog indices, including trimmed vectors"),
                            "input_alias_outputs": "bound to their drivers to avoid unrelated cut inputs",
                            "bound_correspondence_signals": sorted(bound)}}
        result["scope"]["canonical_cuts"] = canonical_cuts
        result["scope"]["isolate_cuts"] = isolate_cuts
        result["scope"]["amended_output_signals"] = sorted(amended)
        if message:
            result["diagnostic"] = {"code": "BENCH_LEC_UNVERIFIED", "message": message}
        run.save(directory / "result.json", result)
        return result

    def execute(argv, name, file_limit=128 * 1024**2):
        remaining = seconds - (time.monotonic() - start)
        if remaining <= 0:
            return {"status": "timeout"}
        result = run.execute(argv, directory, name, seconds=remaining, memory_gib=memory_gib,
                             max_file_bytes=file_limit,
                             monitor=lambda: tree_budget(directory / "eqy-work",
                                                         max_bytes=max_output_gib * 1024**3))
        result["phase"] = name
        steps.append(result)
        return result

    try:
        if len(state_wires) > 128 or (state_wires and not state_only):
            raise ValueError('at most 128 optional wires may accompany state-only matching')
        if strategy.startswith("sat-"):
            return finish("unverified", message="EQY native SAT strategy failed an output-corruption control; use an SBY strategy")
        resolved_tools = []
        for executable in (yosys, eqy, abc):
            found = shutil.which(str(executable))
            if found is None:
                return finish('unverified', message='requested external tool is missing: ' + str(executable))
            # EQY runs its child tools from the proof directory. A relative
            # path that worked for our own subprocess would otherwise fail.
            resolved_tools.append(str(Path(found).resolve()))
        yosys, eqy, abc = resolved_tools
        tool_path = os.pathsep.join([*(str(Path(p).resolve().parent) for p in (yosys, eqy, abc)),
                                    os.environ.get("PATH", "")])
        requested = (["rIC3"] if strategy.startswith("ric3") else [])
        if strategy.endswith("-smt") or strategy.startswith("smt-"):
            requested.append({"yices": "yices-smt2", "bitwuzla": "bitwuzla", "z3": "z3"}[smt_solver])
        for name in requested:
            executable = shutil.which(name, path=tool_path)
            if executable is None:
                return finish("unverified", message="requested external solver is missing: " + name)
            solvers[name] = {"path": executable, "sha256": run.digest(executable),
                             "version": run.version(executable, "--version")}
        equiv.source_limits(config["design"]["files"])
        top = equiv.object_name(config["design"]["top"])
        # The frontend can consume full_case while reading a process, before
        # producing RTLIL. Strip optional promises at the lexical boundary.
        case_sources = []
        priority_files = []
        for index, name in enumerate(config["design"]["files"]):
            path = directory / f"reference-priority-{index}.v"
            case_sources.append(rtl_semantics.verilog_priority(name, path))
            priority_files.append(str(path))
        frontend_config = {**config, "design": {**config["design"], "files": priority_files}}
        lower, gold, gate = equiv.lower_script(frontend_config, directory)
        scripts = lower.read_text().split("design -reset\n")
        if len(scripts) != 2:
            raise equiv.AdaptationError("unexpected structural lowering script")
        scripts = ["\n".join(line for line in script.splitlines()
                             if not line.startswith("write_verilog ")) + "\n"
                   for script in scripts]
        if multiclock:
            scripts = [script.replace("async2sync\n", "") for script in scripts]
        # Case attributes are optional synthesis promises. Keep the input RTL's
        # procedural semantics in the oracle, including arbitrary initial states
        # that can make decoder signals overlap before any instruction is loaded.
        before_proc, separator, after_proc = scripts[0].partition("proc\n")
        if not separator:
            raise equiv.AdaptationError("missing reference process lowering boundary")
        raw_cases = directory / "reference-case-promises.il"
        priority_cases = directory / "reference-priority.il"
        case_script = directory / "reference-cases.ys"
        case_budget = 1024**3 if config.get('rtl_icells') else rtl_semantics.MAX_BYTES
        case_script.write_text(before_proc + compact_functional_names(top, config.get('rtl_icells'))
                               + f"write_rtlil {equiv.quote(raw_cases)}\n")
        case_step = execute([yosys, "-Q", "-T", "-s", str(case_script)], "reference-cases",
                            case_budget)
        if case_step["status"] != "ok":
            return finish(case_step["status"], message="reference case-semantics preparation failed")
        case_normalization = rtl_semantics.priority_cases(raw_cases, priority_cases, max_bytes=case_budget)
        case_normalization["verilog_sources"] = case_sources
        case_normalization["source_semantics_complete"] = True
        run.save(directory / "case-normalization.json", case_normalization)
        scripts[0] = f"read_rtlil {equiv.quote(priority_cases)}\nproc\n" + after_proc
        if not config.get('gate_rtl', False):
            dual_model, dual_manifest = library_models.generate(config['library'], config['netlist'], directory)
            if dual_manifest['models']:
                library_models.certify(config['library'], directory, yosys, abc,
                    seconds=min(120, seconds-(time.monotonic()-start)), memory_gib=min(4,memory_gib))
                scripts[1] = scripts[1].replace('\nread_verilog ',
                    f'\nread_verilog -overwrite {equiv.quote(dual_model)}\nread_verilog ', 1)
        if multiclock:
            # GF180's Liberty stores the complement of the latch's logical Q.
            # Prove a pin-equivalent behavioral model before substituting it;
            # this lets the clock recognizer see the same latch polarity as RTL.
            cell = ("gf180mcu_fd_sc_mcu7t5v0__latq_1" if "gf180mcu" in config["library"]
                    else "sky130_fd_sc_hd__dlxtp_1" if "sky130" in config["library"]
                    else "LATQX1" if "ez130_8t" in config["library"] else None)
            if cell is None:
                raise equiv.AdaptationError("no certified clock-latch model for this library")
            model = run.LEC_FIXTURES / ("lec-latches-ez130.v" if "ez130_8t" in config["library"] else "lec-latches.v")
            component = directory / "latch-model"
            component.mkdir()
            library_script = (
                f"read_liberty -ignore_miss_func -ignore_miss_dir {equiv.quote(config['library'])}\n"
                + f"hierarchy -check -top {cell}\nclk2fflogic\nscc -expect 0\n")
            behavioral_script = (f"read_verilog {equiv.quote(model)}\nhierarchy -check -top {cell}\n"
                                 + "proc\nclk2fflogic\nscc -expect 0\n")
            directions = {}
            for direction, left, right in (("forward", library_script, behavioral_script),
                                           ("reverse", behavioral_script, library_script)):
                component_cfg = component / (direction + ".eqy")
                component_cfg.write_text("[gold]\n" + left + "[gate]\n" + right
                                         + "[strategy pdr]\nuse sby\nengine abc pdr\ntimeout 30\n")
                component_work = component / direction
                check = execute([eqy, "-j", "1", "--yosys", yosys, "--abc", abc,
                                 "-d", str(component_work), str(component_cfg)], "latch-model-" + direction)
                if check["status"] != "ok" or not (component_work / "PASS").exists():
                    return finish(check["status"], message="clock-latch model equivalence was not proved")
                component_parts = [line.split()[1] for line in
                                   (component_work / "partition.list").read_text().splitlines() if line.strip()]
                component_status = strategy_results(component_work, component_parts)
                if not all(p["proven"] for p in component_status.values()):
                    raise equiv.AdaptationError("incomplete clock-latch model proof")
                directions[direction] = {"config_sha256": run.digest(component_cfg),
                                         "partitions": component_status}
            run.save(component / "certificate.json", {
                "cell": cell, "library_sha256": run.digest(config["library"]),
                "model_sha256": run.digest(model), "directions": directions, "verdict": "proven"})
            scripts[1] = scripts[1].replace("\nread_verilog ",
                f"\nread_verilog -overwrite {equiv.quote(model)}\nread_verilog ", 1)
        # Export only the top-level gate declarations for naming metadata. Do not
        # serialize expanded Liberty internals (hundreds of MiB on cache designs).
        gate_front = scripts[1].split("\nproc\n", 1)[0] + "\n"
        if config.get('gate_rtl', False):
            # Internal composition proofs may compare a behavioral diagnostic
            # model. Measured gate-netlist proofs leave this option disabled.
            gate_front += 'proc\n'
        gold_render = f"write_verilog -noattr -norename {equiv.quote(gold)}\n"
        gate_render = f"select {top}\nwrite_verilog -selected -noattr -norename {equiv.quote(gate)}\n"
        if config.get('rtl_icells'):
            if len(config['design']['files']) != 1:
                raise equiv.AdaptationError('internal-cell reference metadata needs a single flat source')
            shutil.copyfile(config['design']['files'][0], gold)
            gold_render = ''
        if config.get('gate_icells'):
            shutil.copyfile(config['netlist'], gate)
            gate_render = ''
        gold_metadata, gate_metadata = directory / "gold-names.json", directory / "gate-names.json"
        private_metadata = (f'delete {top}/w:_diag_* {top}/w:__ideal_mul_* {top}/w:*$*\n'
                            f'setattr -unset src -unset hdlname -unset keep {top}/w:*\n')
        gold_metadata_cleanup = private_metadata if config.get('rtl_icells') else ''
        gate_metadata_cleanup = private_metadata if config.get('gate_icells') else ''
        lower.write_text(scripts[0] + gold_render
                         + f"delete {top}/c:*\nselect {top}\n"
                         + gold_metadata_cleanup
                         + f"write_json -selected {equiv.quote(gold_metadata)}\n"
                         + "design -reset\n" + gate_front
                         + gate_render
                         + f"delete {top}/c:*\nselect {top}\n"
                         + gate_metadata_cleanup
                         + f"write_json -selected {equiv.quote(gate_metadata)}\n")
        lowered = execute([yosys, "-Q", "-T", "-s", str(lower)], "lower", MAX_METADATA_BYTES)
        if lowered["status"] != "ok":
            return finish(lowered["status"], message="external Verilog lowering did not finish")
        for path in (gold, gate):
            if path.stat().st_size > equiv.MAX_BYTES:
                raise equiv.AdaptationError("lowered Verilog byte budget reached")
        for path in (gold_metadata, gate_metadata):
            if path.stat().st_size > MAX_METADATA_BYTES:
                raise equiv.AdaptationError('top-level metadata byte budget reached')
        gold_text, gate_text = gold.read_text(), gate.read_text()
        if config.get('rtl_icells'):
            gold_text = internal_metadata_text(gold_text)
        if config.get('gate_icells'):
            gate_text = internal_metadata_text(gate_text)
        if port_shapes(gold_text, top) != port_shapes(gate_text, top):
            raise equiv.AdaptationError("gold/gate port widths or directions differ")
        port_map, port_commands = port_namespace((gold_text, gate_text), top) if position_names else ({}, "")
        reverse_ports = {proof: original for original, proof in port_map.items()}
        run.save(directory / "port-map.json", port_map)
        register_hints = directory / "register-names.json"
        run.save(register_hints, sorted(register_names(renamed_port_metadata(gold_text, port_map), top,
                                                      positions=position_names)))
        gold_names = direct_model_names(renamed_port_metadata(gold_text, port_map), top,
                                        start + seconds, include_ports=True,
                                        zero_scalar_only=not position_names)
        gate_names = direct_model_names(renamed_port_metadata(gate_text, port_map), top,
                                        start + seconds, include_ports=True,
                                        zero_scalar_only=not position_names)
        outputs = output_names(gold_text, top)
        if outputs != output_names(gate_text, top):
            raise equiv.AdaptationError("gold/gate output port names differ")
        # Extract connectivity from the *same flattened models* passed to EQY.
        # Pre-flatten cell-pin names can alias nets that disappear or merge during
        # expansion; using them as proof cutpoints can recreate false relations.
        gate_finish = scripts[1].split("\nproc\n", 1)[1]
        retained_top_names = ""
        if compact_library_wires:
            # Preserve original top-level names but drop the hundreds of
            # thousands of pin aliases introduced by flattening Liberty cells.
            retained_top_names = f"setattr -set keep 1 {top}/w:*\n"
            if config.get('gate_icells'):
                retained_top_names += f'setattr -unset keep {top}/w:_diag_* {top}/w:__ideal_mul_* {top}/w:*$*\n'
            gate_finish = gate_finish.replace("\nopt_clean\n", "\nopt_clean -purge\n")
        gold_model, gate_model = directory / "gold.il", directory / "gate.il"
        normalize = directory / "normalize.ys"
        compact_metadata = (f"delete {top}/c:*\n"
                            f"delete {top}/w:_[0-9]* {top}/w:*$*\n"
                            f"setattr -unset src -unset hdlname -unset keep -unset capacitance {top}/w:*\n"
                            f"select {top}\n")
        mark_state = ("select -set lec_state_cells t:$*ff*\n"
                      "setattr -set lec_state 1 @lec_state_cells %x:+[Q] @lec_state_cells %d %a\n"
                      + f"select {top}\n")
        model_scripts = [scripts[0] + port_commands + "splitnets -ports\n" + gold_names + "opt_clean\n" + mark_state,
                         gate_front + "proc\n" + port_commands + "splitnets -ports\n" + gate_names + retained_top_names
                         + gate_finish + "opt_clean\n"]
        if multiclock and not (model_only and native_storage):
            # Liberty imports use fine-grained gates and FFs. Serializing their
            # expressions/always blocks and re-reading them gives the external
            # clock-pattern recognizer the same coarse cells as the RTL side.
            # Preserve attributes, especially init: -noattr would lose state.
            clock_files = [directory / "gold-clock.v", directory / "gate-clock.v"]
            export = directory / "clock-expressions.ys"
            export.write_text(scripts[0] + f"write_verilog -norename {equiv.quote(clock_files[0])}\n"
                              + "design -reset\n" + scripts[1]
                              + f"write_verilog -norename {equiv.quote(clock_files[1])}\n")
            converted = execute([yosys, "-Q", "-T", "-s", str(export)], "clock-expressions", 1024**3)
            if converted["status"] != "ok":
                return finish(converted["status"], message="clock expression normalization did not finish")
            model_scripts = []
            for path in clock_files:
                if path.stat().st_size > equiv.MAX_BYTES:
                    raise equiv.AdaptationError("clock expression byte budget reached")
                names = direct_model_names(renamed_port_metadata(path.read_text(), port_map), top,
                                           start + seconds, include_ports=True,
                                           zero_scalar_only=not position_names)
                clock_cuts = ("select -set lec_clock_cells t:$*ff*\n"
                              "setattr -set lec_state 1 @lec_clock_cells %x:+[CLK,C] @lec_clock_cells %d %a\n"
                              + f"select {top}\n") if partition_clocks and path == clock_files[0] else ""
                model_scripts.append(f"read_verilog -sv {equiv.quote(path)}\n"
                                     + f"hierarchy -check -top {top}\nproc\n" + port_commands
                                     + "splitnets -ports\n" + names
                                     + "opt -keepdc\n" + (mark_state if path == clock_files[0] else "")
                                     + clock_cuts
                                     + ("" if partition_clocks else "clk2fflogic\n")
                                     + "scc -expect 0\n")
        normalize.write_text(
            model_scripts[0]
            + compact_functional_names(top, config.get('rtl_icells'))
            + f"write_rtlil {equiv.quote(gold_model)}\n" + compact_metadata
            + f"write_json -selected {equiv.quote(gold_metadata)}\ndesign -reset\n"
            + model_scripts[1]
            + compact_functional_names(top, config.get('gate_icells'))
            + f"write_rtlil {equiv.quote(gate_model)}\n" + compact_metadata
            + f"write_json -selected {equiv.quote(gate_metadata)}\n")
        normalized = execute([yosys, "-Q", "-T", "-s", str(normalize)], "normalize", 1024**3)
        if normalized["status"] != "ok":
            return finish(normalized["status"], message="external model normalization did not finish")
        if any(p.stat().st_size > MAX_METADATA_BYTES for p in (gold_metadata, gate_metadata)):
            raise equiv.AdaptationError("normalized metadata byte budget reached")
        if model_only:
            return finish("prepared", message="formal models prepared; no equivalence verdict yet")
        # An output aliasing an input is not an independent cut input. Other
        # correspondence points remain separate obligations that EQY must prove.
        aliases = input_alias_outputs(gold_text, top) | input_alias_outputs(gate_text, top)
        candidate_file = directory / "correspondence-candidates.json"
        metadata_command = [sys.executable, str(run.HERE / "lec_metadata.py"),
                            "--gold", str(gold_metadata), "--gate", str(gate_metadata),
                            "--top", top, "--output", str(candidate_file),
                            "--group-bits", str(group_bits), "--register-hints", str(register_hints)]
        for pattern in exclude_internal:
            metadata_command += ["--exclude-internal", pattern]
        for pattern in state_wires:
            metadata_command += ['--state-wire', pattern]
        if state_only:
            metadata_command += ["--state-only"]
        if port_aliases_only:
            metadata_command += ["--port-aliases-only"]
        if canonical_cuts:
            metadata_command += ["--canonical-cuts"]
        metadata_seconds = min(180, seconds - (time.monotonic() - start))
        if metadata_seconds <= 0:
            return finish("timeout", message="proof deadline reached before metadata analysis")
        analyzed = run.execute(metadata_command, directory, "metadata",
                               seconds=metadata_seconds, memory_gib=min(4, memory_gib))
        analyzed["phase"] = "metadata"
        steps.append(analyzed)
        if analyzed["status"] != "ok":
            return finish(analyzed["status"], message="bounded correspondence metadata analysis failed")
        if candidate_file.stat().st_size > MAX_METADATA_BYTES:
            raise equiv.AdaptationError("correspondence result byte budget reached")
        candidate_data = run.read_json(candidate_file)
        canonical_ports = candidate_data["ports"]
        expected_bits = {direction: 0 for direction in ("input", "output", "inout")}
        actual_bits = dict(expected_bits)
        for direction, width in port_shapes(gold_text, top).values():
            expected_bits[direction] += width
        for direction in canonical_ports.values():
            actual_bits[direction] += 1
        if expected_bits != actual_bits:
            raise equiv.AdaptationError("bitwise proof interface does not cover every original port bit")
        outputs = {name for name, direction in canonical_ports.items() if direction in ("output", "inout")}

        def matches_port(port, pattern):
            base = re.sub(r"\[[0-9]+\]$", "", port)
            logical = reverse_ports.get(base, base) + port[len(base):]
            return (logical == pattern or reverse_ports.get(base, base) == pattern
                    or fnmatch.fnmatchcase(logical, pattern)
                    or fnmatch.fnmatchcase(reverse_ports.get(base, base), pattern))

        aliases = {port for port in outputs if any(matches_port(port, name) for name in aliases)}
        amended = {port for port in outputs for pattern in amend_outputs if matches_port(port, pattern)}
        if canonical_cuts:
            amended.update(candidate_data["combinational_outputs"])
        connectivity_bound = set(candidate_data["candidates"])
        # Redundant internal aliases are optional correspondence points. Removing
        # those matches retains their logic but avoids requiring independent
        # values for the same wire, and avoids huge bind-driven merge chains.
        # Observable ports are never excluded: retain their drivers instead.
        excluded_matches = (connectivity_bound - canonical_ports.keys()
                            if prune_aliases else set())
        excluded_matches.update(candidate_data["configured_exclusions"])
        connectivity_bound -= excluded_matches
        groups = [[name for name in group if name not in excluded_matches]
                  for group in candidate_data["word_groups"]]
        groups = [group for group in groups if len(group) > 1]
        if canonical_cuts:
            groups.extend(candidate_data["output_alias_groups"])
            kept = candidate_data["canonical_matches"]
            if not set(canonical_ports) <= set(kept):
                raise equiv.AdaptationError("canonical cuts would omit a port")
            cleanup = directory / "canonical-wires.ys"
            commands, inputs = [], {}
            for label, model in (("gold", gold_model), ("gate", gate_model)):
                marked = directory / f"{label}-canonical-input.il"
                inputs[label] = run.digest(model)
                lec_canonical.annotate(model, marked, kept)
                commands.append(lec_canonical.script(marked, model, top))
            cleanup.write_text("".join(commands))
            cleaned = execute([yosys, "-Q", "-T", "-s", str(cleanup)], "canonical-wires", 1024**3)
            if cleaned["status"] != "ok":
                return finish(cleaned["status"], message="canonical wire cleanup did not finish")
            for model in (gold_model, gate_model):
                lec_canonical.verify_names(model, kept)
            run.save(directory / "canonical-wires.json", {
                "before_sha256": inputs,
                "after_sha256": {"gold": run.digest(gold_model), "gate": run.digest(gate_model)},
                "retained_names": len(kept), "output_alias_groups": candidate_data["output_alias_groups"]})
        run.save(directory / "correspondence.json", {
            "gold_metadata_sha256": run.digest(gold_metadata),
            "gate_metadata_sha256": run.digest(gate_metadata),
            "gold_model_sha256": run.digest(gold_model),
            "gate_model_sha256": run.digest(gate_model),
            "constant_or_alias_bindings": sorted(connectivity_bound),
            "configured_internal_exclusions": list(exclude_internal),
            "word_groups": groups,
            "redundant_internal_matches_excluded": sorted(excluded_matches)})
        bound = aliases | connectivity_bound | {port for port in outputs for pattern in bind_outputs
                                                if matches_port(port, pattern)}
        for text in (gold_text, gate_text):
            for line in equiv.declaration_lines(text, top):
                match = equiv.DECL.fullmatch(line)
                if match:
                    name = match[4].removeprefix("\\")
                    if name not in port_map and equiv.OBJECT.fullmatch(name) and any(fnmatch.fnmatchcase(name, p) for p in bind_wires):
                        bound.add(name)
        if len(bound) > MAX_BINDINGS:
            raise equiv.AdaptationError("bound correspondence signal budget reached")
        bind = ("\n[collect *]\nbind " + name_pattern(bound, r"(?:\[[0-9]+\])?")
                + "\n") if bound else ""
        match_rules = ("\n[match *]\ngold-nomatch "
                       + name_pattern(excluded_matches, r"(?:\[[0-9]+\])?")
                       + "\n") if excluded_matches else ""
        if sum(2 * len(name.encode()) + 1 for group in groups for name in group) > MAX_RULE_BYTES:
            raise equiv.AdaptationError("group rule byte budget reached")
        grouping = ("\n[collect *]\n" + "".join(
            "group " + name_pattern(group) + "\n"
            for group in groups)) if groups else ""
        amendment = ("\n[partition *]\namend " + name_pattern(amended) + "\n") if amended else ""
        # RTLIL preserves internal latch/FF cell types across process boundaries.
        # Ordinary Verilog exports above are used only for port/name metadata.
        cfg = directory / "check.eqy"
        cfg.write_text(
            f"[gold]\nread_rtlil {equiv.quote(gold_model)}\n"
            + f"\n[gate]\nread_rtlil {equiv.quote(gate_model)}\n"
            + ("\n[collect *]\nsolo *\n" if isolate_cuts else "")
            + bind + match_rules + grouping + amendment
            + (f"\n[strategy smt_first]\nuse sby\nengine smtbmc {smt_solver}\ndepth 40\ntimeout {partition_seconds}\n"
               + ("option multiclock on\n" if multiclock else "")
               if strategy.startswith("smt-") else "")
            + (f"\n[strategy ric3]\nuse sby\nengine aiger rIC3 -e "
               + ("kind --preproc false" if strategy.startswith("ric3-kind") else "ic3")
               + f"\ntimeout {partition_seconds}\n"
               + ("option multiclock on\n" if multiclock else "")
               if strategy.startswith("ric3") else "")
            + f"\n[strategy pdr]\nuse sby\nengine abc pdr\ntimeout {partition_seconds}\n"
            + ("option multiclock on\n" if multiclock else "")
            + (f"\n[strategy induction]\nuse sby\nengine smtbmc {smt_solver}\ndepth 20\ntimeout {partition_seconds}\n"
               + ("option multiclock on\n" if multiclock else "") if strategy.endswith("-smt") else "")
        )
        del gold_text, gate_text
        work = directory / "eqy-work"
        command = [eqy, "-j", str(jobs), "--yosys", yosys, "--abc", abc, "-d", str(work)]
        setup = execute([*command, "-m", str(cfg)], "eqy-setup", 1024**3)
        if setup["status"] != "ok":
            return finish(setup["status"], message="EQY partition setup did not finish")
        with (work / "partition.list").open() as f:
            for line in f:
                if line.strip():
                    partitions.append(line.split()[1])
                    if len(partitions) > max_partitions:
                        raise equiv.AdaptationError("EQY partition count budget reached")
        if not partitions:
            raise equiv.AdaptationError("EQY generated no proof obligations")
        if multiclock:
            run.save(directory / "clock-scripts.json", partition_clock_scripts(work))
        tree_budget(work, max_bytes=max_output_gib * 1024**3)
        # EQY's generated Makefile runs the same independent strategies without
        # repeating both expensive frontends and partition generation via --continue.
        # Require a PASS certificate for EVERY generated partition, even if make exits 0.
        tool_dirs = [str(Path(path).resolve().parent) for path in (eqy, yosys, abc)]
        path = os.pathsep.join(dict.fromkeys([*tool_dirs, *os.environ.get("PATH", "").split(os.pathsep)]))
        result = execute(["env", "PATH=" + path, "make", "--no-print-directory", "-j", str(jobs),
                          "-C", str(work), "-f", "strategies.mk"], "eqy-prove", 1024**3)
        tree_budget(work, max_bytes=max_output_gib * 1024**3)
        statuses = strategy_results(work, partitions)
        passed = result["status"] == "ok" and all(p["proven"] for p in statuses.values())
        run.save(directory / "strategy-certificate.json", {
            "config_sha256": run.digest(cfg), "partition_list_sha256": run.digest(work / "partition.list"),
            "all_partitions_proven": passed, "partitions": statuses})
        return finish(result["status"], passed,
                      None if passed else "EQY did not prove every partition; inspect proof logs")
    except (OSError, ValueError) as error:
        return finish("unverified", message=str(error))


def main(args):
    measured = run.read_json(args.measurements / "results.json")
    designs = {d["name"]: d for d in measured["inputs"]}
    records = {"schema": 1, "date": datetime.now(timezone.utc).date().isoformat(),
               "method": METHOD, "tools": {"eqy": run.version(args.eqy),
                                              "yosys": run.version(args.yosys, "-V")},
               "rows": []}
    if args.toolchain_manifest:
        if args.toolchain_manifest.stat().st_size > 1024**2:
            raise ValueError("verification toolchain manifest exceeds its byte budget")
        manifest = run.read_json(args.toolchain_manifest)
        for name, path in (("yosys", args.yosys), ("eqy", args.eqy), ("abc", args.abc)):
            entry = manifest["tools"][name]
            if run.digest(path) != entry["sha256"]:
                raise ValueError("verification binary differs from its manifest: " + name)
            if name in records["tools"] and records["tools"][name] != entry["version"]:
                raise ValueError("verification version differs from its manifest: " + name)
        if len(manifest.get("files", {})) > 512:
            raise ValueError("verification runtime manifest file-count budget reached")
        for path, digest in manifest.get("files", {}).items():
            if run.digest(path) != digest:
                raise ValueError("verification runtime file differs from its manifest")
        records["verification_toolchain"] = {"manifest_sha256": run.digest(args.toolchain_manifest),
                                             "package_sha256": manifest.get("package_sha256")}
        records["measured_synthesis_tools"] = measured["tools"]
    else:
        if records["tools"]["yosys"] != measured["tools"]["yosys"]:
            raise ValueError("reference Yosys differs from the pinned synthesis measurement")
        if records["tools"]["eqy"] != run.MANIFEST["reference_versions"]["eqy"]:
            raise ValueError("EQY differs from the pinned LEC version")
    for row in measured["rows"]:
        design = designs[row["design"]]
        if args.design and design["id"] not in args.design:
            continue
        if args.library and row["library"] not in args.library:
            continue
        for tool in ("rustsys", "yosys"):
            if args.tool and tool not in args.tool:
                continue
            source = args.measurements / design["id"] / row["library"] / f"{tool}-{len(row[tool]['samples'])}"
            config = run.read_json(source / "config.json")
            netlist = source / "netlist.v"
            if run.digest(netlist) != row[tool]["samples"][-1]["netlist_sha256"]:
                raise ValueError("measured netlist changed")
            if run.digest(config["library"]) != row["library_sha256"]:
                raise ValueError("measured Liberty changed")
            for path in design["files"]:
                if run.digest(path) != design["sha256"][Path(path).name]:
                    raise ValueError("reference RTL changed")
            directory = args.output / design["id"] / row["library"] / tool
            directory.mkdir(parents=True, exist_ok=False)
            config = {**config, "netlist": str(netlist)}
            run.save(directory / "input.json", config)
            if args.proof_mode == "transition":
                import transition
                proof = transition.prove(config, directory, args.yosys, args.eqy, args.abc,
                                         seconds=args.timeout, memory_gib=args.memory_gib,
                                         limits=transition.Limits(max_cells=args.transition_max_cells,
                                                                  max_state=args.transition_max_state_bits,
                                                                  max_batch_outputs=args.transition_batch_outputs,
                                                                  whole_attempt_s=args.transition_whole_attempt,
                                                                  cec_engine=args.transition_cec_engine,
                                                                  constant_exclusions=tuple(args.transition_unpair_constant)))
            else:
                proof = prove(config, directory, args.yosys, args.eqy, args.abc,
                              args.timeout, args.memory_gib, args.max_partitions, args.partition_timeout,
                              args.strategy, args.bind_output, args.bind_wire, args.multiclock, args.jobs,
                              args.max_output_gib, args.prune_aliases, args.exclude_internal,
                              args.compact_library_wires, args.group_bits, args.state_only, args.amend_output,
                              args.smt_solver, args.partition_clocks, args.port_aliases_only, args.canonical_cuts,
                              args.isolate_cuts)
            records["rows"].append({"design": row["design"], "library": row["library"],
                                    "tool": tool, "netlist_sha256": run.digest(netlist),
                                    "proof": proof})
            run.save(args.output / "lec-results.json", records)
            print(row["design"], row["library"], tool, proof["verdict"],
                  f"{proof['wall_s']:.2f}s", proof.get("diagnostic", ""), flush=True)
    if not records["rows"]:
        raise ValueError("no measured netlists matched the requested filters")
    return 0 if all(row["proof"]["verdict"] == "proven" for row in records["rows"]) else 2


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--measurements", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--eqy", required=True)
    p.add_argument("--yosys", required=True)
    p.add_argument("--abc", required=True)
    p.add_argument("--proof-mode", choices=("sequential", "transition"), default="sequential")
    p.add_argument("--toolchain-manifest", type=Path,
                   help="Explicitly pin a verification toolchain independently of timed synthesis tools")
    p.add_argument("--transition-max-cells", type=int, default=1_100_000)
    p.add_argument("--transition-max-state-bits", type=int, default=200_000)
    p.add_argument("--transition-batch-outputs", type=int, choices=(1, 8, 16, 32, 64), default=32)
    p.add_argument("--transition-whole-attempt", type=int, default=600)
    p.add_argument("--transition-cec-engine", choices=("gia", "legacy"), default="gia")
    p.add_argument('--transition-unpair-constant', action='append', default=[],
                   help='Drop an optional constant-state correspondence; its original state remains universally quantified')
    p.add_argument("--timeout", type=int, default=10800)
    p.add_argument("--memory-gib", type=int, choices=range(1, 17), default=8,
                   help="Per-child address-space cap; use an outer cgroup for aggregate physical RAM")
    p.add_argument("--max-partitions", type=int, default=20000)
    p.add_argument("--partition-timeout", type=int, default=600)
    p.add_argument("--strategy", choices=("pdr", "pdr-smt", "ric3-pdr-smt", "ric3-kind-pdr-smt", "smt-pdr"), default="pdr-smt")
    p.add_argument("--smt-solver", choices=("yices", "bitwuzla", "z3"), default="yices")
    p.add_argument("--bind-output", action="append", default=[],
                   help="Keep matching output cones inside consuming partitions; adds logic, no assumptions")
    p.add_argument("--amend-output", action="append", default=[],
                   help="Add output-driver context to consumers; the producing partition must also pass")
    p.add_argument("--bind-wire", action="append", default=[],
                   help="Keep matching internal signal cones inside consuming partitions")
    p.add_argument("--multiclock", action="store_true", help="Use SBY clock-edge modeling for derived clocks")
    p.add_argument("--partition-clocks", action="store_true",
                   help="Keep proved clock cutpoints and sample clock edges inside each partition")
    p.add_argument("--prune-aliases", action="store_true",
                   help="Exclude redundant internal alias matchpoints instead of binding their cones")
    p.add_argument("--exclude-internal", action="append", default=[],
                   help="Remove an optional internal correspondence name/pattern; never removes a port")
    p.add_argument("--compact-library-wires", action="store_true",
                   help="Discard unused flattened Liberty pin aliases while preserving top-level names")
    p.add_argument("--group-bits", type=int, choices=(0, 8, 16, 32, 64), default=0,
                   help="Group adjacent bits in bounded proof partitions; zero leaves EQY defaults")
    p.add_argument("--state-only", action="store_true",
                   help="Use edge-triggered state and primary ports as matchpoints; retain other logic")
    p.add_argument("--port-aliases-only", action="store_true",
                   help="Bind duplicate output bits and constants; leave other internal alias matches to EQY")
    p.add_argument("--canonical-cuts", action="store_true",
                   help="Use one port-preferred state cut per wire, collapse aliases and retain combinational output context")
    p.add_argument("--isolate-cuts", action="store_true",
                   help="Disable automatic fragment grouping; retain explicit bounded word and alias groups")
    p.add_argument("--jobs", type=int, choices=range(1, 5), default=1,
                   help="Concurrent proof strategies; bound aggregate RAM with an outer cgroup")
    p.add_argument("--max-output-gib", type=int, choices=range(1, 33), default=8)
    p.add_argument("--design", action="append", choices=[d["id"] for d in run.MANIFEST["designs"]])
    p.add_argument("--library", action="append", choices=run.MANIFEST["libraries"])
    p.add_argument("--tool", action="append", choices=("rustsys", "yosys"))
    args = p.parse_args()
    if min(args.timeout, args.max_partitions, args.partition_timeout) <= 0:
        p.error("all budgets must be positive")
    if args.partition_clocks and not args.multiclock:
        p.error("--partition-clocks requires --multiclock")
    if args.proof_mode == "transition" and (args.bind_output or args.bind_wire or args.amend_output
            or args.exclude_internal or args.multiclock or args.partition_clocks or args.prune_aliases
            or args.compact_library_wires or args.group_bits or args.state_only or args.port_aliases_only
            or args.canonical_cuts or args.isolate_cuts or args.strategy != "pdr-smt" or args.jobs != 1):
        p.error("transition mode constructs its own state interface; omit sequential partition/strategy options")
    args.measurements, args.output = args.measurements.resolve(), args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=False)
    raise SystemExit(main(args))
