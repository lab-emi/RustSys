#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Bounded, serial black-box benchmark; Python is only a development dependency.

No reference-tool source is read. See README.md for the upfront memory estimate.
Every RustEDA mutation uses a fresh disk workspace and an explicit base revision.
"""
import argparse
from datetime import datetime, timezone
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import shutil
import signal
import statistics
import subprocess
import sys
import time
import tomllib

import rtl_semantics

HERE = Path(__file__).resolve().parent
# Measured drivers also run from a frozen snapshot below .work/. Find the
# owning checkout rather than interpreting the snapshot depth as the root.
ROOT = next(p for p in HERE.parents if (p / 'Cargo.toml').is_file()
            and (p / 'verification/benchmarks/open-hardware/designs.json').is_file())
LEC_FIXTURES = ROOT / "verification/fixtures/synthetic/lec"
MANIFEST = json.loads((HERE / "designs.json").read_text())
MAX_FILE = 128 * 1024 * 1024


def digest(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def read_json(path):
    if path.stat().st_size > MAX_FILE:
        raise ValueError(f"oversized report: {path}")
    return json.loads(path.read_text())


def limits(memory_gib, max_file_bytes=MAX_FILE):
    # The CPU-only children and their descendants inherit these hard limits.
    if memory_gib:
        n = memory_gib * 1024**3
        inherited = resource.getrlimit(resource.RLIMIT_AS)[1]
        if inherited != resource.RLIM_INFINITY:
            n = min(n, inherited)
        resource.setrlimit(resource.RLIMIT_AS, (n, n))
    file_limit = resource.getrlimit(resource.RLIMIT_FSIZE)[1]
    file_limit = max_file_bytes if file_limit == resource.RLIM_INFINITY else min(max_file_bytes, file_limit)
    resource.setrlimit(resource.RLIMIT_FSIZE, (file_limit, file_limit))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


def execute(argv, directory, name, seconds=180, memory_gib=8, haskell=False,
            max_file_bytes=MAX_FILE, monitor=None, worker_threads=1):
    """Kill the whole process group, including ABC, on deadline exhaustion."""
    if not 0 < max_file_bytes <= 4 * 1024**3:
        raise ValueError("output file limit must be between one byte and 4 GiB")
    directory.mkdir(parents=True, exist_ok=True)
    save(directory / f"{name}.command.json", argv)
    with open(directory / f"{name}.stdout", "wb") as out, open(
        directory / f"{name}.stderr", "wb"
    ) as err:
        start = time.monotonic()
        # GHC reserves a large virtual address range even for a small heap. Use
        # a physical-memory cgroup for sv2v; never cap its virtual address space.
        guarded = (["systemd-run", "--user", "--scope", "--quiet", "-p", f"MemoryMax={memory_gib}G",
                    "-p", "MemorySwapMax=1G", *argv] if haskell else argv)
        process = subprocess.Popen(
            ["/usr/bin/time", "-v", "-o", str(directory / f"{name}.time"),
             "timeout", "--kill-after=2s", f"{seconds}s", *guarded],
            stdout=out, stderr=err, stdin=subprocess.DEVNULL,
            start_new_session=True, preexec_fn=lambda: limits(None if haskell else memory_gib, max_file_bytes),
            env={**os.environ, "LC_ALL": "C", "OMP_NUM_THREADS": str(worker_threads),
                 "RAYON_NUM_THREADS": str(worker_threads), "RUST_TEST_THREADS": "2"},
        )
        budget_error = None

        def terminate_group():
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()

        try:
            if monitor is None:
                code = process.wait(timeout=seconds + 3)
            else:
                deadline = start + seconds + 3
                while True:
                    monitor()
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise subprocess.TimeoutExpired(argv, seconds)
                    try:
                        code = process.wait(timeout=min(5, remaining))
                        break
                    except subprocess.TimeoutExpired:
                        if time.monotonic() >= deadline:
                            raise
            status = "ok" if code == 0 else "timeout" if code == 124 else "failed"
        except subprocess.TimeoutExpired:
            terminate_group()
            code, status = 124, "timeout"
        except (ValueError, OSError) as error:
            terminate_group()
            code, status, budget_error = 125, "resource_limit", str(error)
        elapsed = time.monotonic() - start
    t = directory / f"{name}.time"
    text = t.read_text() if t.exists() else ""
    rss = re.search(r"Maximum resident set size \(kbytes\): (\d+)", text)
    wall = re.search(r"Elapsed \(wall clock\) time .*: ([0-9:.]+)", text)
    measured = 0.0
    if wall:
        for part in wall[1].split(":"):
            measured = measured * 60 + float(part)
    if status == "ok" and (not wall or not rss):
        status = "missing_metrics"
    result = {"status": status, "exit_code": code, "wall_s": measured if wall else None,
              "guard_wall_s": elapsed,
              "peak_rss_kib": int(rss[1]) if rss else None}
    user = re.search(r"User time \(seconds\): ([0-9.]+)", text)
    system = re.search(r"System time \(seconds\): ([0-9.]+)", text)
    result.update(user_cpu_s=float(user[1]) if user else None,
                  system_cpu_s=float(system[1]) if system else None)
    result['average_cpu_cores'] = ((float(user[1]) + float(system[1])) / measured
                                   if user and system and measured > 0 else None)
    if budget_error is not None:
        result["diagnostic"] = {"code": "BENCH_RESOURCE_LIMIT", "message": budget_error}
    save(directory / f"{name}.status.json", result)
    return result


def checked(argv, directory, name, **kwargs):
    result = execute(argv, directory, name, **kwargs)
    if result["status"] != "ok":
        raise RuntimeError(f"{name}: {result['status']}; see {directory}")
    return result


def version(binary, option="--version"):
    try:
        return subprocess.check_output([binary, option], text=True, timeout=10).strip()
    except (OSError, subprocess.SubprocessError) as error:
        raise RuntimeError(f"UNVERIFIED: cannot run {binary}: {error}") from error


def prepare(args):
    inputs = []
    for design in MANIFEST["designs"]:
        repo = args.workspace / design["id"]
        if not repo.exists():
            checked(["git", "clone", "--no-checkout", "--filter=blob:none", "--depth=1",
                     design["repo"] + ".git", str(repo)], args.workspace, f"fetch-{design['id']}")
        head = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip()
        if head != design["commit"] or not (repo / "LICENSE").exists():
            checked(["git", "-C", str(repo), "fetch", "--depth=1", "origin", design["commit"]],
                    args.workspace, f"pin-{design['id']}")
            checked(["git", "-C", str(repo), "checkout", "--detach", design["commit"]],
                    args.workspace, f"checkout-{design['id']}")
        if subprocess.check_output(["git", "-C", str(repo), "status", "--porcelain"], text=True):
            raise ValueError(f"upstream checkout is modified: {repo}")
        dest = args.workspace / "prepared" / design["id"]
        dest.mkdir(parents=True, exist_ok=True)
        if design.get("prepare") == "ibex_sv2v":
            if version(args.sv2v) != MANIFEST["reference_versions"]["sv2v"]:
                raise ValueError("sv2v version differs from the pinned reference")
            prim = repo / "vendor/lowrisc_ip/ip/prim/rtl"
            generic = repo / "vendor/lowrisc_ip/ip/prim_generic/rtl"
            sources = [prim / (n + ".sv") for n in (
                "prim_cipher_pkg", "prim_secded_pkg", "prim_mubi_pkg", "prim_util_pkg", "prim_ram_1p_pkg")]
            sources += [repo / "rtl/ibex_pkg.sv"]
            for name in ("buf", "clock_gating", "flop"):
                source = generic / f"prim_generic_{name}.sv"
                bound = dest / f"prim_{name}.sv"
                # Same vendor hardware, only the primitive binding's module name changes.
                bound.write_text(source.read_text().replace(
                    f"module prim_generic_{name}", f"module prim_{name}"))
                sources.append(bound)
            sources += [prim / "prim_lfsr.sv"]
            exclude = {"ibex_pkg.sv", "ibex_tracer_pkg.sv", "ibex_tracer.sv",
                       "ibex_core_tracing.sv", "ibex_top_tracing.sv"}
            sources += sorted(p for p in (repo / "rtl").glob("*.sv") if p.name not in exclude)
            includes = [repo / "rtl", prim, repo / "vendor/lowrisc_ip/dv/sv/dv_utils"]
            cmd = [args.sv2v, "-DSYNTHESIS", "--top=ibex_top", *[f"-I{p}" for p in includes],
                   *map(str, sources)]
            checked(cmd, dest, "sv2v", seconds=args.timeout, haskell=True)
            shutil.copyfile(dest / "sv2v.stdout", dest / "ibex.v")
            files = [dest / "ibex.v"]
        else:
            files = []
            for pattern in design["sources"]:
                for source in sorted(repo.glob(pattern)):
                    target = dest / source.name
                    shutil.copyfile(source, target)
                    files.append(target)
            if not files:
                raise ValueError(f"no sources for {design['id']}")
        inputs.append({**design, "files": [str(p) for p in files],
                       "sha256": {p.name: digest(p) for p in files}})
    save(args.workspace / "inputs.json", inputs)


def quote(path):
    return '"' + str(path).replace("\\", "\\\\").replace('"', '\\"') + '"'


def latch_map(config):
    if config.get('latch_map'):return Path(config['latch_map'])
    library=str(config['library'])
    if 'sky130' in library:return LEC_FIXTURES/'latches-sky130.v'
    if 'gf180' in library:return LEC_FIXTURES/'latches-gf180.v'
    raise ValueError('explicit latch map required for this library')


def pipeline(config):
    """Child entrypoint: include all CLI startup, disk IO and mmap in the timer."""
    c = read_json(Path(config))
    directory, lib = Path(c["directory"]), c["library"]
    design = c["design"]
    if c["tool"] == "rustsys":
        ws = directory / "ws"
        common = ["--workspace", str(ws), "--threads", str(c.get('threads', 1)), "--format", "json"]
        commands = [
            [c["rusteda"], *common, "init"],
            [c["rusteda"], *common, "import", "--base-rev", "rev0", "--lib", lib,
             "--corner", c["corner"], "--top", design["top"], "--rtl", *design["files"]],
            [c["rustsys"], *common, "--progress", "off", "synth", "--base-rev", "rev1",
             "--backend", "cpu", "--effort", "medium", "--goal", "area"],
            [c["rusteda"], *common, "export", "--rev", "rev2", "--netlist", str(directory / "netlist.v")],
        ]
        save(directory / "pipeline.json", commands)
        for command in commands:
            code = subprocess.call(command, stdin=subprocess.DEVNULL)
            if code:
                return code
        reports = list(ws.glob("revisions/*/synth.json"))
        if len(reports) != 1:
            raise ValueError("expected one synthesis report")
        shutil.copyfile(reports[0], directory / "report.json")
    else:
        # Include process startup, parsing, and the explicit case-semantics
        # normalization in the measured pipeline. No reset is assumed.
        read_design = "read_verilog -sv " + " ".join(map(quote, design["files"])) + "\n"
        if rtl_semantics.contains_case_promises(design["files"]):
            priority_files, case_sources = [], []
            for index, name in enumerate(design["files"]):
                path = directory / f"priority-source-{index}.v"
                case_sources.append(rtl_semantics.verilog_priority(name, path))
                # Stable logical filenames keep frontend-generated wire names
                # identical across timed repetitions in different directories.
                priority_files.append(path.name)
            read_design = "read_verilog -sv " + " ".join(map(quote, priority_files)) + "\n"
            raw = directory / "source-case-promises.il"
            priority = directory / "source-priority.il"
            frontend = directory / "source.ys"
            frontend.write_text(read_design
                + f"hierarchy -check -top {design['top']}\nwrite_rtlil {quote(raw)}\n")
            code = subprocess.call([c["yosys"], "-Q", "-T", "-s", str(frontend)],
                                   stdin=subprocess.DEVNULL, cwd=directory)
            if code:
                return code
            case_policy = rtl_semantics.priority_cases(raw, priority)
            case_policy.update(verilog_sources=case_sources, source_semantics_complete=True)
            save(directory / "case-normalization.json", case_policy)
            read_design = f"read_rtlil {quote(priority)}\n"
        script = directory / "synth.ys"
        no_share = " -noshare" if c.get('yosys_noshare', False) else ""
        script.write_text(
            f"read_liberty -lib {quote(lib)}\n"
            + read_design
            + f"synth -top {design['top']} -flatten -noabc -nofsm -nordff{no_share}\n"
            f"techmap -map {quote(latch_map(c))}\n"
            "techmap\n"
            f"dfflibmap -liberty {quote(lib)}\n"
            f"abc -liberty {quote(lib)}\n"
            # Keep explicit RTL register boundaries for arbitrary-state LEC.
            # Complete combinational X/undriven bits, without -init or resets.
            "clean\nsetundef -zero -undriven\ncheck -assert\n"
            f"tee -o {quote(directory / 'report.json')} stat -json -liberty {quote(lib)}\n"
            f"write_verilog -noattr {quote(directory / 'netlist.v')}\n"
        )
        quiet = ["-q"] if c.get('quiet_yosys', False) else []
        return subprocess.call([c["yosys"], "-Q", "-T", *quiet, "-s", str(script)], stdin=subprocess.DEVNULL)
    return 0


def qos(directory, tool):
    r = read_json(directory / "report.json")
    if tool == "rustsys":
        return {"cells": sum(r["cells"].values()), "area_um2": r["area"],
                "cell_counts": r["cells"], "warnings": r["warnings"],
                "state_bits": r["state_bits_retained"], "latches": r["latches"]}
    r = r["design"]
    cells = r.get("num_cells_by_type", {})
    # $scopeinfo is Yosys hierarchy/debug metadata, not a gate. Every other
    # internal cell is an unmapped logic element and invalidates the comparison.
    unmapped = {k: v for k, v in cells.items() if k.startswith("$") and k != "$scopeinfo"}
    return {"cells": r["num_cells"] - cells.get("$scopeinfo", 0), "area_um2": r.get("area"),
            "cell_counts": {k: v for k, v in cells.items() if k != "$scopeinfo"}, "unmapped": unmapped}


def equivalence(config, directory, args):
    if args.proof_method == "canonical":
        import equiv
        proof_dir = directory / args.proof_label
        proof_dir.mkdir(exist_ok=False)
        adapted = {**config, "netlist": str(directory / "netlist.v")}
        result = equiv.prove(adapted, proof_dir, args.yosys, execute,
                             seconds=args.proof_timeout, memory_gib=args.memory_gib)
        result["log_dir"] = args.proof_label
        save(proof_dir / "result.json", result)
        return result
    design, lib = config["design"], config["library"]
    # Independent netlist-to-RTL check, outside the synthesis timer. Never use
    # ignored unknown cells, black-box stubs, or an empty proof as a success.
    script = directory / "equiv.ys"
    script.write_text(
        "read_verilog -sv " + " ".join(map(quote, design["files"])) + "\n"
        f"hierarchy -check -top {design['top']}\nproc\nflatten\nmemory\nopt\nasync2sync\n"
        f"rename {design['top']} gold\ndesign -stash gold\n"
        f"read_liberty -ignore_miss_func -ignore_miss_dir {quote(lib)}\n"
        f"read_verilog {quote(directory / 'netlist.v')}\n"
        f"hierarchy -check -top {design['top']}\nproc\nflatten\nmemory\nopt\nasync2sync\n"
        f"rename {design['top']} gate\ndesign -copy-from gold gold\n"
        "equiv_make gold gate equiv\nhierarchy -check -top equiv\n"
        "equiv_simple -short -seq 1\nequiv_induct -seq 4\nequiv_status -assert\n"
    )
    result = execute([args.yosys, "-Q", "-T", "-s", str(script)], directory, "equiv",
                     seconds=args.proof_timeout, memory_gib=args.memory_gib)
    log = (directory / "equiv.stdout").read_text()
    proven = re.search(r"Found (\d+) \$equiv cells", log)
    success = (result["status"] == "ok" and proven and int(proven[1]) > 0
               and "Equivalence successfully proven!" in log)
    result["verdict"] = "proven" if success else "unverified"
    return result


def summarize(samples):
    if any(s["status"] != "ok" for s in samples):
        return {"status": "unverified", "samples": samples}
    q = [s["qor"] for s in samples]
    if any(x != q[0] for x in q) or len({s["netlist_sha256"] for s in samples}) != 1:
        return {"status": "nondeterministic", "samples": samples}
    return {"status": "ok", "median_wall_s": statistics.median(s["wall_s"] for s in samples),
            "peak_rss_kib": max(s["peak_rss_kib"] for s in samples), "qor": q[0],
            "samples": samples}


def verify(args):
    """Re-read measured artifacts and retry proofs without repeating timings."""
    out = args.workspace / args.label
    result = read_json(out / "results.json")
    if version(args.yosys, "-V") != result["tools"]["yosys"]:
        raise ValueError("UNVERIFIED: reference tool changed since measurement")
    designs = {d["name"]: d for d in result["inputs"]}
    for design in designs.values():
        for source in design["files"]:
            if digest(source) != design["sha256"][Path(source).name]:
                raise ValueError(f"UNVERIFIED: input changed since measurement: {source}")
    for row in result["rows"]:
        design = designs[row["design"]]
        if args.design and design["id"] not in args.design:
            continue
        if args.library and row["library"] not in args.library:
            continue
        for tool in ("rustsys", "yosys"):
            if args.tool and tool not in args.tool:
                continue
            old = row[tool]
            samples = old["samples"]
            for i, sample in enumerate(samples, 1):
                directory = out / design["id"] / row["library"] / f"{tool}-{i}"
                if sample["status"] in ("ok", "unmapped_cells"):
                    sample["qor"] = qos(directory, tool)
                    sample["status"] = "unmapped_cells" if sample["qor"].get("unmapped") else "ok"
            aggregate = summarize(samples)
            for key in ("equivalence", "independent_checker", "equivalence_history"):
                if key in old:
                    aggregate[key] = old[key]
            retry = args.force_proofs or (args.retry_unverified and aggregate.get("equivalence", {}).get("verdict") != "proven")
            if aggregate["status"] == "ok" and ("equivalence" not in aggregate or retry):
                directory = out / design["id"] / row["library"] / f"{tool}-{len(samples)}"
                config = read_json(directory / "config.json")
                if digest(config["library"]) != row["library_sha256"]:
                    raise ValueError("UNVERIFIED: PDK changed since measurement")
                if digest(directory / "netlist.v") != samples[-1]["netlist_sha256"]:
                    raise ValueError("UNVERIFIED: measured netlist changed")
                history = list(old.get("equivalence_history", []))
                if "equivalence" in aggregate:
                    history.append(aggregate["equivalence"])
                aggregate["equivalence"] = equivalence(config, directory, args)
                if history:
                    aggregate["equivalence_history"] = history
                print(f"{row['design']} {row['library']} {tool}: {aggregate['equivalence']['verdict']}", flush=True)
            row[tool] = aggregate
            save(out / "results.json", result)


def run(args):
    available = next(int(line.split()[1]) for line in Path("/proc/meminfo").read_text().splitlines()
                     if line.startswith("MemAvailable:"))
    if available < (args.memory_gib + 1) * 1024**2:
        raise RuntimeError("UNVERIFIED: insufficient available host RAM for the requested child limit")
    inputs = read_json(args.workspace / "inputs.json")
    for d in inputs:
        for file in d["files"]:
            if digest(file) != d["sha256"][Path(file).name]:
                raise ValueError(f"source changed since preparation: {file}")
    if version(args.yosys, "-V") != MANIFEST["reference_versions"]["yosys"]:
        raise ValueError("Yosys version differs from the pinned reference")
    out = args.workspace / args.label
    out.mkdir()  # Do not overwrite prior evidence.
    pdks = {x["name"]: x for x in tomllib.loads((ROOT / "verification/pdks.toml").read_text())["library"]}
    code_files = sorted(p for base in (ROOT / "crates",) for p in base.rglob("*")
                        if p.is_file() and p.suffix in (".rs", ".toml", ".cu", ".metal"))
    source_hash = hashlib.sha256()
    for p in [ROOT / "Cargo.toml", ROOT / "Cargo.lock", *code_files]:
        source_hash.update(str(p.relative_to(ROOT)).encode() + b"\0" + p.read_bytes())
    result = {"schema": 1, "date": datetime.now(timezone.utc).date().isoformat(), "inputs": inputs,
              "rustsys_source_sha256": source_hash.hexdigest(),
              "tools": {"rustsys": version(args.rustsys), "yosys": version(args.yosys, "-V"),
                        "rustsys_binary_sha256": digest(args.rustsys),
                        "rusteda_binary_sha256": digest(args.rusteda)},
              "host": {"cpu": next(s.split(":", 1)[1].strip() for s in Path("/proc/cpuinfo").read_text().splitlines()
                                    if s.startswith("model name")),
                       "kernel": os.uname().release, "meminfo": Path("/proc/meminfo").read_text()},
              "limits": {"memory_gib": args.memory_gib, "timeout_s": args.timeout,
                         "proof_timeout_s": args.proof_timeout, "proof_method": args.proof_method,
                         "threads": 1, "repetitions": args.repetitions},
              "scope": {"wire_model": "none (synthesis only)", "corner": "per library",
                        "unconstrained_endpoints": "all; no SDC or STA", "drc_status": "unchecked"},
              "rows": []}
    for design in inputs:
        if args.design and design["id"] not in args.design:
            continue
        for name in MANIFEST["libraries"]:
            if args.library and name not in args.library:
                continue
            pdk = pdks[name]
            lib = args.pdks / f"{pdk['library']}__{pdk['corner']}.lib"
            if digest(lib) != pdk["sha256"]:
                raise ValueError(f"PDK checksum mismatch: {lib}")
            row = {"design": design["name"], "library": name, "corner": pdk["corner"],
                   "library_sha256": pdk["sha256"]}
            last = {}
            # Interleave the two tools each repetition to reduce time/order drift.
            samples = {"rustsys": [], "yosys": []}
            for rep in range(args.repetitions + 1):
                for tool in ("rustsys", "yosys") if rep % 2 == 0 else ("yosys", "rustsys"):
                    directory = out / design["id"] / name / f"{tool}-{rep}"
                    directory.mkdir(parents=True)
                    config = {"directory": str(directory), "tool": tool, "library": str(lib),
                              "corner": pdk["corner"], "design": design,
                              "rustsys": args.rustsys, "rusteda": args.rusteda, "yosys": args.yosys}
                    save(directory / "config.json", config)
                    sample = execute([sys.executable, str(Path(__file__).resolve()), "_pipeline",
                                      str(directory / "config.json")], directory, "synth",
                                     seconds=args.timeout, memory_gib=args.memory_gib)
                    if sample["status"] == "ok":
                        sample["qor"] = qos(directory, tool)
                        sample["netlist_sha256"] = digest(directory / "netlist.v")
                        if sample["qor"].get("unmapped"):
                            sample["status"] = "unmapped_cells"
                    print(f"{design['name']} {name} {tool} {'warmup' if rep == 0 else rep}: "
                          f"{sample['status']} {sample['guard_wall_s']:.3f}s", flush=True)
                    if rep:
                        samples[tool].append(sample)
                    last[tool] = (config, directory)
            for tool in ("rustsys", "yosys"):
                row[tool] = summarize(samples[tool])
                if row[tool]["status"] == "ok" and not args.timings_only:
                    config, directory = last[tool]
                    row[tool]["equivalence"] = equivalence(config, directory, args)
                    if tool == "rustsys":
                        c = execute([args.rustsys, "--threads", "1", "--progress", "off", "check", "synth",
                                     "--top", design["top"], "--lib", str(lib), "--netlist", str(directory / "netlist.v"),
                                     "--budget", "100000", "--model-budget", "100000", "--report", str(directory / "check.json"),
                                     *design["files"]], directory, "check", seconds=args.proof_timeout,
                                    memory_gib=args.memory_gib)
                        c["verdict"] = read_json(directory / "check.json").get("verdict") if (directory / "check.json").exists() else "unverified"
                        row[tool]["independent_checker"] = c
                    print(f"  {tool} external equivalence: {row[tool]['equivalence']['verdict']}", flush=True)
                elif args.timings_only:
                    row[tool]["equivalence"] = {"status": "not_run", "verdict": "unverified"}
                    if tool == "rustsys":
                        row[tool]["independent_checker"] = {"status": "not_run", "verdict": "unverified"}
            result["rows"].append(row)
            save(out / "results.json", result)
    return result


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "_pipeline":
        return pipeline(sys.argv[2])
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("prepare", "run", "verify"))
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--pdks", type=Path, default=ROOT / "target/pdk")
    parser.add_argument("--rustsys", default=str(ROOT / "target/release/rustsys"))
    parser.add_argument("--rusteda", default=str(ROOT / "target/release/rusteda"))
    parser.add_argument("--yosys", default="yosys")
    parser.add_argument("--sv2v", default="sv2v")
    parser.add_argument("--label", default="results")
    parser.add_argument("--design", action="append", choices=[d["id"] for d in MANIFEST["designs"]])
    parser.add_argument("--library", action="append", choices=MANIFEST["libraries"])
    parser.add_argument("--tool", action="append", choices=("rustsys", "yosys"))
    parser.add_argument("--proof-method", choices=("canonical", "legacy"), default="canonical")
    parser.add_argument("--proof-label", default="canonical-v1")
    parser.add_argument("--retry-unverified", action="store_true")
    parser.add_argument("--force-proofs", action="store_true", help="Recheck selected netlists under a changed proof scope; retain earlier verdicts")
    parser.add_argument("--timings-only", action="store_true", help="Measure synthesis; leave proof status unverified for a separate proof run")
    parser.add_argument("--repetitions", type=int, choices=range(1, 11), default=3)
    parser.add_argument("--timeout", type=int, default=180)
    parser.add_argument("--proof-timeout", type=int, default=60)
    parser.add_argument("--memory-gib", type=int, choices=range(1, 9), default=8)
    args = parser.parse_args()
    if args.tool and args.action != "verify":
        parser.error("--tool filters verification only; a benchmark run compares both tools")
    if (not re.fullmatch(r"[A-Za-z0-9_-]+", args.label)
        or not re.fullmatch(r"[A-Za-z0-9_-]+", args.proof_label)
        or min(args.timeout, args.proof_timeout) < 1):
        parser.error("label must be shell-safe; timeouts must be positive")
    args.workspace = args.workspace.resolve()
    args.pdks = args.pdks.resolve()
    args.workspace.mkdir(parents=True, exist_ok=True)
    with open(args.workspace / "workspace.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if args.action == "prepare":
            prepare(args)
        elif args.action == "verify":
            verify(args)
        else:
            run(args)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
