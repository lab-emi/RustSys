#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Retry the native checker on copies of measured workspaces.

Memory plan: one core/corner/worker, at most 512 MiB of workspace files and
4096 files. A million graph objects at 256 bytes plus bounded SAT/model state
and allocator overhead gives a conservative 3 GiB estimate. Children have a
4 GiB address-space cap; run this program in a 4 GiB physical-memory cgroup.
The wall deadline and optional sweep limit are configurable. Every final
output/state proof obligation remains mandatory, including with no sweeping.
"""
import argparse
from datetime import datetime, timezone
import fcntl
from pathlib import Path
import shutil

import run
from profile_check import workspace_files


def verify(args):
    available = next(int(line.split()[1]) for line in Path("/proc/meminfo").read_text().splitlines()
                     if line.startswith("MemAvailable:"))
    if available < 5 * 1024**2:
        raise RuntimeError("UNVERIFIED: less than 5 GiB available host RAM")
    measured = run.read_json(args.measurements / "results.json")
    designs = {design["name"]: design for design in measured["inputs"]}
    binary_hash = run.digest(args.rustsys)
    results = {"schema": 1, "date": datetime.now(timezone.utc).isoformat(),
               "checker_binary_sha256": binary_hash,
               "synthesis_binary_sha256": measured["tools"]["rustsys_binary_sha256"],
               "scope": "native X/Z-as-zero checking of copied measured workspaces",
               "limits": {"memory_gib": 4, "wall_s": args.timeout, "threads": 1,
                          "conflicts_per_obligation": args.budget,
                          "model_conflicts_per_obligation": args.budget,
                          "sweep_max_candidates": args.sweep_max_candidates},
               "rows": []}
    for row in measured["rows"]:
        design = designs[row["design"]]
        if design["id"] not in args.design or row["library"] not in args.library:
            continue
        if row["rustsys"]["status"] != "ok":
            raise ValueError("only completed, stable synthesis measurements can be checked")
        sample = args.measurements / design["id"] / row["library"] / f"rustsys-{len(row['rustsys']['samples'])}"
        netlist_hash = run.digest(sample / "netlist.v")
        if netlist_hash != row["rustsys"]["samples"][-1]["netlist_sha256"]:
            raise ValueError("measured netlist changed")
        source = sample / "ws"
        source_hashes = workspace_files(source)
        name = f"{design['id']}-{row['library']}"
        workspace = args.output / name
        shutil.copytree(source, workspace)
        if workspace_files(workspace) != source_hashes:
            raise ValueError("workspace copy differs from measured input")
        if run.digest(args.rustsys) != binary_hash:
            raise ValueError("checker binary changed during verification")
        command = [str(args.rustsys), "--workspace", str(workspace), "--format", "json",
                   "--threads", "1", "--progress", "off", "check", "--base-rev", "rev2",
                   "--budget", str(args.budget), "--model-budget", str(args.budget),
                   "--sweep-max-candidates", str(args.sweep_max_candidates)]
        result = run.execute(command, args.output, name, seconds=args.timeout, memory_gib=4)
        report_path = workspace / "revisions/rev0003/check.json"
        report = run.read_json(report_path) if report_path.exists() else {}
        passed = (result["status"] == "ok" and report.get("verdict") == "equivalent"
                  and report.get("outputs_checked", 0) > 0 and report.get("state_pairs_checked", 0) > 0)
        result.update({"design": row["design"], "id": design["id"], "library": row["library"],
                       "verdict": "equivalent" if passed else "unverified",
                       "netlist_sha256": netlist_hash, "source_workspace_sha256": source_hashes,
                       "source_workspace_unchanged": workspace_files(source) == source_hashes,
                       "outputs_checked": report.get("outputs_checked"),
                       "state_pairs_checked": report.get("state_pairs_checked"),
                       "report_sha256": run.digest(report_path) if report_path.exists() else None,
                       "corner": row["corner"], "wire_model": "none",
                       "unconstrained_endpoints": "all; synthesis checking only"})
        if not result["source_workspace_unchanged"]:
            raise ValueError("measured workspace changed during verification")
        results["rows"].append(result)
        run.save(args.output / "results.json", results)
        print(name, result["status"], result["verdict"], result["wall_s"], flush=True)
    return 0 if results["rows"] and all(row["verdict"] == "equivalent" for row in results["rows"]) else 2


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--measurements", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--rustsys", type=Path, required=True)
    parser.add_argument("--timeout", type=int, default=1800)
    parser.add_argument("--budget", type=int, default=100000)
    parser.add_argument("--sweep-max-candidates", type=int, default=0)
    parser.add_argument("--design", action="append", choices=[d["id"] for d in run.MANIFEST["designs"]])
    parser.add_argument("--library", action="append", choices=run.MANIFEST["libraries"])
    args = parser.parse_args()
    if min(args.timeout, args.budget) <= 0 or args.sweep_max_candidates < 0:
        parser.error("deadlines/budgets must be positive and sweep count nonnegative")
    args.measurements, args.output, args.rustsys = (path.resolve() for path in (args.measurements, args.output, args.rustsys))
    args.design = args.design or [d["id"] for d in run.MANIFEST["designs"]]
    args.library = args.library or list(run.MANIFEST["libraries"])
    args.output.mkdir(parents=True, exist_ok=False)
    with (args.measurements.parent / "workspace.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        return verify(args)


if __name__ == "__main__":
    raise SystemExit(main())
