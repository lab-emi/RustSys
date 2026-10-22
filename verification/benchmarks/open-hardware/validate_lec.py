#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Independent mapped-netlist controls for aliases and feedback clocks.

Memory preflight: <16 state bits, <2,000 logic nodes, one solver at a time.
Expected RSS <256 MiB; cap the suite at 2 GiB RAM and 900 s. Every child has
its own 2 GiB address-space and wall-time limit. Reference tools are opaque.
"""
import argparse
import fcntl
from pathlib import Path
import re
import sys

import lec
import run
import transition

FIXTURES = {
    "async_latches": "module async_latches(input e,r,d,output reg [7:0] q);\n"
        + "\n".join(f"always @* if ({'r' if reset else '!r'}) q[{index}] = 1'b{value}; else if ({'e' if enable else '!e'}) q[{index}] = d;"
                    for index, (enable, reset, value) in enumerate(
                        ( (e, r, v) for e in (0, 1) for r in (0, 1) for v in (0, 1))))
        + "\nendmodule\n",
    "wide_output": """module wide_output(input [63:0] data, output [63:0] q);
  assign q = data;
endmodule
""",
    "aliases": """module aliases(input clk, rst_n, en, input [1:0] data,
  output [1:0] q, output mirror, zero);
  reg [1:0] state;
  reg constant_state;
  wire duplicate = state[0];
  always @(posedge clk) begin
    constant_state <= 0;
    if (!rst_n) state <= 0;
    else if (en) state <= state + data;
  end
  assign q = state;
  assign mirror = duplicate;
  assign zero = constant_state;
endmodule
""",
    "feedback_clock": """module feedback_clock(input clk, rst_n, en, d, output reg q);
  reg enable_latch;
  always @* if (!clk) enable_latch = en | q;
  wire gated_clk = clk & enable_latch;
  always @(posedge gated_clk or negedge rst_n)
    if (!rst_n) q <= 0;
    else q <= d;
endmodule
""",
    "feedback_enable": """module feedback_enable(input clk, rst_n, clock_en, load, d, output reg q);
  reg enable_latch;
  always @* if (!clk) enable_latch = clock_en | q;
  wire gated_clk = clk & enable_latch;
  always @(posedge gated_clk or negedge rst_n)
    if (!rst_n) q <= 0;
    else if (load) q <= d;
endmodule
""",
    "trimmed_indices": """module trimmed_indices(input clk, rst_n,
  input [7:0] data, output [7:0] q);
  reg [15:0] state;
  always @(posedge clk) if (!rst_n) state <= 0;
    else state <= {data, 8'h00};
  assign q = state[15:8];
endmodule
""",
}


def validate(args):
    records = []
    for pdk, filename in (("sky130", "sky130_fd_sc_hd__tt_025C_1v80.lib"),
                          ("gf180", "gf180mcu_fd_sc_mcu7t5v0__tt_025C_3v30.lib")):
        for top, source_text in FIXTURES.items():
            if args.fixture and top not in args.fixture:
                continue
            directory = args.workspace / pdk / top
            directory.mkdir(parents=True, exist_ok=False)
            source = directory / "input.v"
            source.write_text(source_text)
            config = {"tool": "yosys", "directory": str(directory), "yosys": args.yosys,
                      "library": str(args.pdks / filename),
                      "design": {"top": top, "files": [str(source)]}}
            synthesis_config = config
            if top == "trimmed_indices":
                # Same behavior with the unused low byte removed. The public
                # internal indices remain [15:8], as in Yosys's trimmed nets.
                candidate = directory / "trimmed.v"
                candidate.write_text(source_text.replace("reg [15:0] state;", "(* keep *) reg [15:8] state;")
                                     .replace("state <= {data, 8'h00};", "state <= data;"))
                synthesis_config = {**config, "design": {"top": top, "files": [str(candidate)]}}
            run.save(directory / "config.json", synthesis_config)
            run.checked([sys.executable, str(run.HERE / "run.py"), "_pipeline",
                         str(directory / "config.json")], directory, "synth",
                        seconds=60, memory_gib=2)
            netlist = directory / "netlist.v"
            original = netlist.read_text()
            mutant = directory / "mutant.v"
            port = "mirror" if top == "aliases" else "q"
            # Rename every occurrence of the DUT output to a private wire and
            # reconnect only the public output through an inverter. The mapped
            # cell implementation and every state transition remain intact.
            text = re.sub(r"\b" + port + r"\b", "lec_original_output", original)
            text, count = re.subn(r"output (\[[^]]+\]\s+)?lec_original_output;",
                                  lambda m: f"output {m[1] or ''}{port};\n"
                                  f"  wire {m[1] or ''}lec_original_output;\n"
                                  f"  assign {port} = ~lec_original_output;", text)
            if count != 1:
                raise RuntimeError("cannot plant exactly one output inversion")
            # Restore the public port name in the module's argument list only.
            header, rest = text.split(");", 1)
            text = header.replace("lec_original_output", port) + ");" + rest
            if top == "wide_output":
                text = text.replace("assign q = ~lec_original_output;",
                                    "assign q = lec_original_output ^ 64'h8000000000000000;")
            mutant.write_text(text)
            cases = [("positive", netlist, "proven"), ("negative", mutant, "unverified")]
            if top == "aliases":
                cases += [("ports_only_positive", netlist, "proven"),
                          ("ports_only_negative", mutant, "unverified")]
            if top.startswith("feedback_"):
                cases += [("partition_positive", netlist, "proven"),
                          ("partition_negative", mutant, "unverified")]
                # Change actual clock behavior, not just output polarity. Both
                # global and partition-local models must detect the bypass.
                clock_source = directory / "clock-bypass.v"
                clock_source.write_text(source_text.replace(
                    "wire gated_clk = clk & enable_latch;", "wire gated_clk = clk;"))
                clock_dir = directory / "clock-bypass-build"
                clock_dir.mkdir()
                clock_config = {**config, "directory": str(clock_dir),
                                "design": {"top": top, "files": [str(clock_source)]}}
                run.save(clock_dir / "config.json", clock_config)
                run.checked([sys.executable, str(run.HERE / "run.py"), "_pipeline",
                             str(clock_dir / "config.json")], clock_dir, "synth",
                            seconds=60, memory_gib=2)
                cases += [("clock_bypass", clock_dir / "netlist.v", "unverified"),
                          ("partition_clock_bypass", clock_dir / "netlist.v", "unverified")]
            if top == "trimmed_indices":
                # With canonical cuts, incorrect optional state names can be
                # discarded and the unchanged real output interface still
                # proved. This fixture has no planted logic error.
                cases.append(("wrong_index_policy", netlist,
                              "proven" if args.canonical_cuts else "unverified"))
            for name, candidate, expected in cases:
                if args.proof_mode == "transition" and name == "wrong_index_policy":
                    continue  # The separate name-adapter regression above owns this case.
                target = directory / name
                target.mkdir()
                reference = {**config, "netlist": str(candidate)}
                if name == "wrong_index_policy":
                    # Regression control: a Yosys vector [15:8] must not be
                    # renamed to RustSys bit positions [7:0].
                    reference["tool"] = "rustsys"
                if args.proof_mode == "transition":
                    result = transition.prove(reference, target, args.yosys, args.eqy, args.abc,
                                              seconds=180, memory_gib=2,
                                              limits=transition.Limits(whole_attempt_s=0) if args.force_batches else None)
                else:
                    result = lec.prove(reference, target,
                                       args.yosys, args.eqy, args.abc, seconds=120,
                                       memory_gib=2, partition_seconds=20,
                                       strategy=args.strategy, multiclock=top.startswith("feedback_"),
                                       group_bits=32, compact_library_wires=top == "aliases",
                                       exclude_internal=("*",) if name.startswith("ports_only") else (),
                                       state_only=top == "aliases",
                                       amend_outputs=("mirror",) if top == "aliases" else (),
                                       partition_clocks=name.startswith("partition_"),
                                       canonical_cuts=args.canonical_cuts, isolate_cuts=args.isolate_cuts)
                if result["verdict"] != expected:
                    raise RuntimeError(f"unexpected {pdk}/{top}/{name}: {result}")
                if expected == "unverified":
                    if args.proof_mode == "transition":
                        log = target / "cec/cec.stdout"
                        reset_mismatch = (top.startswith("feedback_")
                            and result.get("diagnostic", {}).get("message") == "incompatible asynchronous reset value")
                        # Inverting a resettable output can be rejected before
                        # the Boolean miter: its corresponding reset values
                        # already differ. The separate reset-polarity controls
                        # exercise both positive and negative forms of this gate.
                        if not reset_mismatch and (not log.exists() or "NOT EQUIVALENT" not in log.read_text()):
                            raise RuntimeError("negative transition control lacks an actual CEC counterexample")
                    else:
                        statuses = lec.strategy_results(target / "eqy-work", [
                            line.split()[1] for line in
                            (target / "eqy-work/partition.list").read_text().splitlines() if line.strip()])
                        if not any("FAIL" in r["strategies"].values() for r in statuses.values()):
                            raise RuntimeError("negative control lacks an actual counterexample")
                records.append({"pdk": pdk, "fixture": top, "case": name,
                                "netlist_sha256": run.digest(candidate), "proof": result})
                print(pdk, top, name, result["verdict"], flush=True)
    run.save(args.workspace / "validation.json", records)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--canonical-cuts", action="store_true")
    parser.add_argument("--proof-mode", choices=("sequential", "transition"), default="sequential")
    parser.add_argument("--fixture", choices=tuple(FIXTURES), action="append")
    parser.add_argument("--force-batches", action="store_true",
                        help="Validate explicit output projection instead of the whole-design fast path")
    parser.add_argument("--isolate-cuts", action="store_true")
    parser.add_argument("--strategy", choices=("pdr-smt", "smt-pdr"), default="pdr-smt")
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--pdks", type=Path, required=True)
    parser.add_argument("--yosys", required=True)
    parser.add_argument("--eqy", required=True)
    parser.add_argument("--abc", required=True)
    args = parser.parse_args()
    args.workspace, args.pdks = args.workspace.resolve(), args.pdks.resolve()
    args.workspace.mkdir(parents=True, exist_ok=True)
    with (args.workspace / "workspace.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        validate(args)
