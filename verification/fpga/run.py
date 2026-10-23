#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Bounded RustSys -> native primitives -> Vivado/XSim acceptance campaign.

Missing tools are unverified (exit 2). Functional simulation is a bounded
reset-qualified differential check, never presented as formal equivalence.
All subprocesses have cgroup/OS memory limits, file caps, wall guards and RSS logs.
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import shutil
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
MANIFEST = json.loads((HERE / "ibex.json").read_text())


def digest(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def save(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def tool(value: str) -> Path | None:
    found = shutil.which(value)
    return Path(found).absolute() if found else None


def vivado_path(value: str | None) -> Path | None:
    if value:
        return tool(value)
    candidates = ["vivado"]
    if os.environ.get("XILINX_VIVADO"):
        candidates.append(str(Path(os.environ["XILINX_VIVADO"]) / "bin/vivado"))
    for root in [Path("/tools/Xilinx/Vivado"), Path("/opt/Xilinx/Vivado"),
                 Path("/tools/AMD/Vivado"), Path("/opt/AMD/Vivado"), Path("/eda/Xilinx/Vivado")]:
        candidates.extend(str(p) for p in sorted(root.glob("*/bin/vivado"), reverse=True))
    return next((p for c in candidates if (p := tool(c))), None)


class Campaign:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
        self.out = args.workspace / "verification" / f"fpga-{stamp}"
        self.out.mkdir(parents=True, exist_ok=False)
        self.inputs = self.out / "inputs"
        self.inputs.mkdir()
        harness = {}
        for name in ["run.py", "ibex.json", "probe.sv", "probe_tb.sv", "primitives.v", "ibex_tb.sv", "implement.tcl"]:
            copied = self.inputs / name
            shutil.copyfile(HERE / name,copied)
            harness[name] = {"path": str(copied),"sha256": digest(copied)}
        self.result = {"schema": 1, "status": "unverified", "design": args.design,
                       "harness": harness,
                       "campaign_kind": "ibex_vivado_acceptance" if args.design == "ibex" else "primitive_differential_controls",
                       "part": args.part, "corner": None, "wire_model": "unmodeled",
                       "unconstrained_endpoints": None, "drc_status": "unchecked",
                       "formal_equivalence": "unchecked", "simulation_scope": MANIFEST["simulation_scope"],
                       "physical_scope": MANIFEST["physical_scope"], "stages": {}, "tools": {},
                       "memory_estimate": {"frontend_and_mapping_peak_mib": 1908,
                           "ibex_aig_nodes_estimate": 40000, "retained_cuts_per_node": 8,
                           "cut_entry_bytes": 48, "corners": 1, "mapping_threads": 1,
                           "build_jobs": 2, "test_threads": 2, "simulation_peak_mib": 512,
                           "vivado_peak_mib": 4096, "vivado_timing_corners":2, "max_file_mib": 256}}
        self.scope = shutil.which("systemd-run") and subprocess.run(
            ["systemd-run", "--user", "--scope", "--quiet", "true"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10).returncode == 0
        self.result["resource_guard"] = {"kind":"cgroup" if self.scope else "rlimit_as",
            "wall_seconds_per_stage":args.timeout,"lock_wait_seconds":args.lock_timeout,
            "small_memory_mib":2048,"vivado_memory_mib":8192,"swap_mib":1024}
        self.lock = args.lock_file
        self.lock.parent.mkdir(parents=True, exist_ok=True)
        free = subprocess.run(["free", "-h"], capture_output=True, text=True, timeout=5)
        (self.out / "host-memory.txt").write_text(free.stdout)
        save(self.out / "result.json", self.result)

    def record(self, name: str, status: str, **fields: object) -> None:
        self.result["stages"][name] = {"status": status, **fields}
        save(self.out / "result.json", self.result)

    def run(self, name: str, argv: list[object], *, cwd: Path | None = None,
            marker: str | None = None, negative: bool = False, large: bool = False) -> Path:
        cwd = cwd or self.out
        cwd.mkdir(parents=True, exist_ok=True)
        log = self.out / f"{name}.log"
        timing = self.out / f"{name}.time"
        cmd = [str(x) for x in argv]
        # Timeout starts after the lock is acquired; the process tree is killed together.
        command = ["flock", "-w", str(self.args.lock_timeout), str(self.lock), "timeout", "--kill-after=10s", str(self.args.timeout),
                   "/usr/bin/time", "-v", "-o", str(timing), *cmd]
        if self.scope:
            command = ["systemd-run", "--user", "--scope", "--quiet", "-p",
                       f"MemoryMax={'8G' if large else '2G'}", "-p", "MemorySwapMax=1G", *command]

        def limits() -> None:
            resource.setrlimit(resource.RLIMIT_FSIZE, (256 << 20, 256 << 20))
            if not self.scope:
                cap = (16 if large else 4) << 30
                resource.setrlimit(resource.RLIMIT_AS, (cap, cap))

        start = time.monotonic()
        with log.open("w") as output:
            proc = subprocess.run(command, cwd=cwd, stdout=output, stderr=subprocess.STDOUT,
                                  env={**os.environ, "RUST_TEST_THREADS": "2", "RAYON_NUM_THREADS": "2"},
                                  preexec_fn=limits, check=False)
        # Files are capped; stream instead of reading potentially huge logs into memory.
        matched = marker is None
        mismatch = False
        with log.open(errors="replace") as lines:
            for line in lines:
                matched |= marker is not None and marker in line
                mismatch |= "RUSTSYS_MISMATCH" in line
        rss = None
        if timing.exists():
            found = re.search(r"Maximum resident set size \(kbytes\):\s*(\d+)", timing.read_text())
            if found:
                rss = int(found[1])
        if negative:
            accepted = proc.returncode != 0 and mismatch and not matched
        else:
            accepted = proc.returncode == 0 and matched and not mismatch
        if rss is not None and rss > (8 if large else 2) * 1024 * 1024:
            accepted = False
        self.record(name, "passed" if accepted else "failed", argv=cmd,
                    returncode=proc.returncode, wall_s=time.monotonic()-start,
                    maximum_rss_kib=rss, log=str(log), expected_counterexample=negative)
        if not accepted:
            raise RuntimeError(f"{name} failed; see {log}")
        return log

    def version(self, name: str, path: Path, flag: str) -> None:
        log = self.run(f"version-{name}", [path, flag])
        self.result["tools"][name] = {"path": str(path), "sha256": digest(path),
                                       "version_output": log.read_text()[:4096]}
        expected = MANIFEST["reference_versions"].get(name)
        if expected and expected not in log.read_text().splitlines():
            raise ValueError(f"{name} version differs from the pinned ibex.json reference")

    def simulate_icarus(self, label: str, netlist: Path, reference: Path,
                        *, behavioral: bool = False, negative: bool = False) -> None:
        ibex = self.args.design == "ibex"
        top = "ibex_tb" if ibex else "fpga_probe_tb"
        bench = self.inputs / ("ibex_tb.sv" if ibex else "probe_tb.sv")
        compiled = self.out / f"{label}.vvp"
        command = [self.iverilog, "-g2012", "-DSYNTHESIS", "-s", top, "-o", compiled]
        if behavioral and ibex:
            command += ["-DREFERENCE=ibex_top", netlist, bench]
        else:
            command += [reference, netlist, self.inputs / "primitives.v", bench]
        self.run(f"{label}-compile", command)
        for seed in ([1, 7] if ibex and not negative else [1]):
            self.run(f"{label}-seed{seed}", [self.vvp, compiled, f"+SEED={seed}"],
                     marker=f"RUSTSYS_PASS {'ibex' if ibex else 'probe'}", negative=negative)

    def xsim(self, label: str, netlist: Path, reference: Path, *, timing: bool = False,
             behavioral: bool = False) -> None:
        directory = self.out / label
        command = [self.xvlog, "--sv", "-d", "SYNTHESIS"]
        command += ["-d", "REFERENCE=ibex_top", netlist] if behavioral else [reference, netlist]
        command += [self.inputs / "ibex_tb.sv", self.glbl]
        self.run(f"{label}-compile", command, cwd=directory, large=True)
        command = [self.xelab, "ibex_tb", "glbl", "-s", "checked", "-mt", "2",
                   "-L", "unisims_ver", "-L", "unimacro_ver", "-L", "secureip"]
        if timing:
            sdf = netlist.with_name(netlist.name.replace("_timing.v", ".sdf"))
            if not sdf.is_file() or sdf.stat().st_size == 0:
                raise ValueError(f"missing SDF for timing simulation: {sdf}")
            arcs = {"iopath":0,"interconnect":0}
            with sdf.open(errors="replace") as lines:
                for line in lines:
                    arcs["iopath"] += line.upper().count("(IOPATH")
                    arcs["interconnect"] += line.upper().count("(INTERCONNECT")
            if not arcs["iopath"] or ("post-implementation" in label and not arcs["interconnect"]):
                raise ValueError(f"SDF lacks required cell/interconnect timing arcs: {sdf}")
            command += ["-L", "simprims_ver", "-maxdelay", "-transport_int_delays",
                        "-pulse_r", "0", "-pulse_int_r", "0", "-sdfmax", f"/ibex_tb/dut={sdf}"]
            self.result.setdefault("sdf",{})[label] = {"path":str(sdf),"sha256":digest(sdf),"instance":"/ibex_tb/dut","corner":"slow/max","arcs":arcs}
        elaboration = self.run(f"{label}-elaborate", command, cwd=directory, large=True)
        def check_sdf(log: Path) -> None:
            if timing:
                with log.open(errors="replace") as lines:
                    for line in lines:
                        lower = line.lower()
                        if "sdf" in lower and any(w in lower for w in ["warning","error","failed","cannot","unable"]):
                            raise ValueError(f"SDF annotation diagnostic requires review: {log}")
        check_sdf(elaboration)
        for seed in [1, 7]:
            log = self.run(f"{label}-seed{seed}", [self.xsim_bin, "checked", "-runall", "-testplusarg", f"SEED={seed}"],
                     cwd=directory, marker="RUSTSYS_PASS ibex", large=True)
            check_sdf(log)

    def execute(self) -> int:
        rtl = self.args.rtl if self.args.design == "ibex" else self.inputs / "probe.sv"
        if not rtl or not rtl.is_file():
            raise ValueError("--rtl must point to the prepared Ibex Verilog")
        if self.args.design == "ibex" and digest(rtl) != MANIFEST["prepared_sha256"]:
            raise ValueError("prepared Ibex hash differs from pinned ibex.json")
        self.result["source"] = {"path": str(rtl), "sha256": digest(rtl)}
        for name, path in [("rustsys",self.args.rustsys),("rusteda",self.args.rusteda)]:
            if not path.is_file(): raise ValueError(f"missing {name} executable: {path}")
            self.version(name,path,"--version")
        self.iverilog, self.vvp = tool(self.args.iverilog), tool(self.args.vvp)
        if not self.iverilog or not self.vvp:
            self.record("icarus", "unverified", reason="Icarus/vvp not found")
            return 2
        self.version("iverilog", self.iverilog, "-V")
        self.version("vvp", self.vvp, "-V")
        ws = self.args.workspace
        if not (ws / "manifest.json").exists():
            if self.args.base_rev != "rev0": raise ValueError("new workspace requires --base-rev rev0")
            self.run("init", [self.args.rusteda, "-w", ws, "--format", "json", "init"])
        top = "ibex_top" if self.args.design == "ibex" else "fpga_probe"
        imported = self.run("import", [self.args.rusteda, "-w", ws, "--format", "json", "import",
                                    "--base-rev", self.args.base_rev, "--top", top, "--rtl", rtl])
        base = json.loads(imported.read_text())["new_rev"]
        log = self.run("synthesis", [self.args.rustsys, "-w", ws, "--threads", "2", "--format", "json",
                                   "synth-fpga", "--base-rev", base, "--part", self.args.part])
        envelope = json.loads(log.read_text())
        mapped = Path(envelope["artifacts"]["netlist"])
        self.result["synthesis"] = envelope["report"]
        self.result["netlist"] = {"path": str(mapped), "sha256": digest(mapped)}
        reference = self.out / "reference.v"
        reference.write_text(re.sub(r"\bmodule\s+" + top + r"\b",
            "module " + ("ibex_reference" if self.args.design == "ibex" else "fpga_probe_reference"), rtl.read_text(), count=1))
        if self.args.design == "ibex":
            self.simulate_icarus("behavioral", rtl, reference, behavioral=True)
        self.simulate_icarus("mapped-functional", mapped, reference)
        if self.args.design == "probe":
            mutations = {
                "bad-lut": lambda s: re.sub(r"\.INIT\(2'h1\)", ".INIT(2'h2)", s, count=1),
                "bad-reset": lambda s: s.replace("IS_CLR_INVERTED(1'b1)", "IS_CLR_INVERTED(1'b0)", 1),
                "bad-clock": lambda s: s.replace("IS_C_INVERTED(1'b1)", "IS_C_INVERTED(1'b0)", 1),
            }
            text = mapped.read_text()
            for name, mutate in mutations.items():
                changed = mutate(text)
                if text == changed: raise ValueError(f"negative control {name} did not mutate the netlist")
                bad = self.out / f"{name}.v"; bad.write_text(changed)
                self.simulate_icarus(name, bad, reference, negative=True)
            self.result["status"] = "passed"
            return 0
        vivado = vivado_path(self.args.vivado)
        if not vivado:
            for stage in ["vivado-implementation", "xsim-behavioral", "xsim-post-synthesis-functional",
                          "xsim-post-synthesis-timing", "xsim-post-implementation-functional", "xsim-post-implementation-timing"]:
                self.record(stage, "unverified", reason="Vivado installation not found")
            return 2
        self.version("vivado",vivado,"-version")
        # Require an explicit pin for acceptance; discovery never silently selects a version.
        if not self.args.vivado_version or self.args.vivado_version not in self.result["tools"]["vivado"]["version_output"]:
            raise ValueError("--vivado-version must match the installed Vivado version output")
        self.xvlog, self.xelab, self.xsim_bin = [vivado.parent / name for name in ["xvlog","xelab","xsim"]]
        root = Path(os.environ.get("XILINX_VIVADO", str(vivado.parent.parent)))
        self.glbl = root / "data/verilog/src/glbl.v"
        if not all(p.is_file() for p in [self.xvlog,self.xelab,self.xsim_bin,self.glbl]):
            raise ValueError("incomplete Vivado/XSim installation; source settings64.sh before running")
        impl = self.out / "implementation"
        self.run("vivado-implementation", [vivado,"-mode","batch","-nojournal","-nolog","-source",
                  self.inputs / "implement.tcl","-tclargs",mapped,self.args.part,impl],
                 marker="RUSTSYS_IMPLEMENTED",large=True)
        self.result["implementation"] = json.loads((impl / "implementation.json").read_text())
        self.result.update(corner="slow/fast min_max; SDF slow/max",wire_model="routed",drc_status="checked")
        self.xsim("xsim-behavioral",rtl,reference,behavioral=True)
        for phase in ["post_synthesis","post_implementation"]:
            for kind in ["functional","timing"]:
                self.xsim(f"xsim-{phase.replace('_','-')}-{kind}",impl / f"{phase}_{kind}.v",reference,timing=kind=="timing")
        self.result["status"] = "passed"
        return 0


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--workspace",required=True,type=Path)
    p.add_argument("--base-rev",required=True)
    p.add_argument("--design",choices=["ibex","probe"],default="ibex")
    p.add_argument("--rtl",type=Path)
    p.add_argument("--rustsys",required=True,type=Path)
    p.add_argument("--rusteda",required=True,type=Path)
    p.add_argument("--iverilog",default="iverilog")
    p.add_argument("--vvp",default="vvp")
    p.add_argument("--vivado")
    p.add_argument("--vivado-version",help="exact expected version line or release identifier")
    p.add_argument("--part",default=MANIFEST["default_part"])
    p.add_argument("--timeout",type=int,default=600)
    p.add_argument("--lock-file",type=Path,default=ROOT / ".work/scale.lock")
    p.add_argument("--lock-timeout",type=int,default=60)
    a = p.parse_args()
    for name in ["workspace","rtl","rustsys","rusteda","lock_file"]:
        if getattr(a,name) is not None: setattr(a,name,getattr(a,name).resolve())
    if a.timeout <= 0 or a.lock_timeout <= 0 or not re.fullmatch(r"rev[0-9]+",a.base_rev): p.error("positive timeouts and revN base required")
    campaign = Campaign(a)
    try:
        code = campaign.execute()
    except (OSError,ValueError,RuntimeError,subprocess.SubprocessError) as e:
        campaign.result.update(status="failed", error=str(e)); code=1
    save(campaign.out / "result.json",campaign.result)
    print(json.dumps({"status":campaign.result["status"],"report":str(campaign.out / "result.json")}))
    return code


if __name__ == "__main__":
    sys.exit(main())
