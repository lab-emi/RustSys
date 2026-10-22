#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Serial before/after full-workspace checker profiling on measured netlists.

Memory plan: one corner and child at a time, <=512 MiB copied workspace, <=4096
files; parent metadata <64 MiB. Checker estimate <=3 GiB, capped at 4 GiB/90 s.
Run under the exclusive scale lock; GNU time records child peak RSS. No new RTL
graphs are generated. Copies and hashing are outside the timed command.
"""
import argparse
from datetime import datetime, timezone
import fcntl
import os
from pathlib import Path
import shutil
import statistics

import run


def workspace_files(source):
    files, size = [], 0
    for path in source.rglob("*"):
        if path.is_symlink():
            raise ValueError("workspace symlinks are not accepted")
        if not path.is_file():
            continue
        files.append(path)
        size += path.stat().st_size
        if len(files) > 4096 or size > 512 * 1024**2:
            raise ValueError("workspace copy budget reached")
    return {str(path.relative_to(source)): run.digest(path) for path in sorted(files)}


def profile(args):
    available = next(int(line.split()[1]) for line in Path("/proc/meminfo").read_text().splitlines()
                     if line.startswith("MemAvailable:"))
    if available < 5 * 1024**2:
        raise RuntimeError("UNVERIFIED: less than 5 GiB available host RAM")
    measured = run.read_json(args.measurements / "results.json")
    designs = {d["name"]: d for d in measured["inputs"]}
    binaries = {"before": args.before, "after": args.after}
    binary_hashes = {key: run.digest(path) for key, path in binaries.items()}
    result = {"schema": 1, "date": datetime.now(timezone.utc).date().isoformat(),
              "host": {"cpu": next(line.split(":", 1)[1].strip()
                                   for line in Path("/proc/cpuinfo").read_text().splitlines()
                                   if line.startswith("model name")),
                       "kernel": os.uname().release},
              "binaries": binary_hashes,
              "scope": "full rev2 workspace check through report revision commit; copies excluded",
              "limits": {"memory_gib": 4, "timeout_s": 90, "threads": 1,
                         "repetitions": args.repetitions, "conflicts_per_obligation": 100000},
              "rows": []}
    for measured_row in measured["rows"]:
        design = designs[measured_row["design"]]
        if design["id"] not in args.design or measured_row["library"] not in args.library:
            continue
        if measured_row["rustsys"]["status"] != "ok":
            raise ValueError("only stable, completed synthesis measurements can be profiled")
        samples = measured_row["rustsys"]["samples"]
        directory = args.measurements / design["id"] / measured_row["library"] / f"rustsys-{len(samples)}"
        source = directory / "ws"
        if run.digest(directory / "netlist.v") != samples[-1]["netlist_sha256"]:
            raise ValueError("measured mapped netlist changed")
        config = run.read_json(directory / "config.json")
        if run.digest(config["library"]) != measured_row["library_sha256"]:
            raise ValueError("measured library changed")
        row = {"design": design["id"], "library": measured_row["library"],
               "scope": {"corner": measured_row["corner"], "wire_model": "none",
                         "unconstrained_endpoints": "all; synthesis checking only"},
               "qor": measured_row["rustsys"]["qor"],
               "netlist_sha256": samples[-1]["netlist_sha256"],
               "source_workspace_sha256": workspace_files(source), "samples": []}
        result["rows"].append(row)
        for repeat in range(args.repetitions):
            for label in ("before", "after") if repeat % 2 == 0 else ("after", "before"):
                binary = binaries[label]
                if run.digest(binary) != binary_hashes[label]:
                    raise ValueError("checker binary changed during profiling")
                name = f"{design['id']}-{measured_row['library']}-{repeat + 1}-{label}"
                ws = args.output / name
                shutil.copytree(source, ws)
                if workspace_files(ws) != row["source_workspace_sha256"]:
                    raise ValueError("workspace copy differs from the common input")
                argv = [str(binary), "--workspace", str(ws), "--format", "json", "--threads", "1",
                        "--progress", "off", "check", "--base-rev", "rev2", "--budget", "100000",
                        "--model-budget", "100000"]
                sample = run.execute(argv, args.output, name, seconds=90, memory_gib=4)
                report_path = ws / "revisions/rev0003/check.json"
                report = run.read_json(report_path) if report_path.exists() else {}
                sample.update({"tool": label, "repeat": repeat + 1,
                               "verdict": report.get("verdict", "unverified"),
                               "outputs_checked": report.get("outputs_checked"),
                               "state_pairs_checked": report.get("state_pairs_checked"),
                               "sweep": report.get("sweep")})
                row["samples"].append(sample)
                run.save(args.output / "comparison.json", result)
                print(name, sample["status"], sample["verdict"], sample["wall_s"], flush=True)
                if sample["status"] != "ok" or sample["verdict"] != "equivalent":
                    raise RuntimeError("UNVERIFIED: incomplete checker result; see saved logs")
        counts = {(s["outputs_checked"], s["state_pairs_checked"]) for s in row["samples"]}
        if len(counts) != 1 or not all(counts.pop()):
            raise RuntimeError("UNVERIFIED: checker obligation counts differ or are empty")
        row["median_wall_s"] = {
            label: statistics.median(s["wall_s"] for s in row["samples"] if s["tool"] == label)
            for label in binaries}
        run.save(args.output / "comparison.json", result)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--measurements", type=Path, required=True)
    parser.add_argument("--before", type=Path, required=True)
    parser.add_argument("--after", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repetitions", type=int, choices=range(1, 11), default=3)
    parser.add_argument("--design", action="append", choices=[d["id"] for d in run.MANIFEST["designs"]])
    parser.add_argument("--library", action="append", choices=run.MANIFEST["libraries"])
    args = parser.parse_args()
    args.measurements, args.before, args.after = (p.resolve() for p in (args.measurements, args.before, args.after))
    args.output = args.output.resolve()
    args.design = args.design or ["ibex", "serv"]
    args.library = args.library or run.MANIFEST["libraries"]
    args.output.mkdir(parents=True, exist_ok=False)
    # Do not race an ongoing synthesis/proof update of the measured workspaces.
    with open(args.measurements.parent / "workspace.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        profile(args)
