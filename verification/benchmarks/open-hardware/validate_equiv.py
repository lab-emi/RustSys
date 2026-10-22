#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Dual-PDK positive and planted-error checks of external state-name adaptation.

Memory plan: fewer than 32 state bits / 2,000 logic nodes, one corner and worker;
under 256 MiB expected RSS including Liberty. Children are capped at 2 GiB / 60 s.
The fixture is generated here; reference tools are black-box executables only.
"""
import argparse
import fcntl
import json
from pathlib import Path
import re
import sys

import equiv
import lec
import run

RTL = """module state_shapes(input clk, rst_n, input [2:0] data,
  output [2:0] result, output reg [4:2] mirror, output parity, x_choice,
  output [1:0] x_arithmetic, output reg match_case);
  reg [0:0] carry;
  reg [4:2] count;
  reg [0:2] ascending;
  reg [0:0] bank [3:4];
  always @(posedge clk) begin
    if (!rst_n) begin
      carry <= 0; count <= 0; ascending <= 0;
      bank[3] <= 0; bank[4] <= 0; mirror <= 0;
    end else begin
      carry <= data[0] ^ carry;
      count <= count + data;
      ascending <= {ascending[1:2], data[1]};
      bank[3] <= data[2]; bank[4] <= bank[3] ^ data[0];
      mirror <= {carry, data[1:0]};
    end
  end
  assign result = count ^ ascending ^ {bank[4], bank[3], carry};
  assign parity = (data[0] & ~data[1]) | (~data[0] & data[1]);
  assign x_choice = data[2] ? carry : 1'bx;
  assign x_arithmetic = 2'b0x + 2'b01;
  always @* casez (data)
    3'b1??: match_case = 1'b1;
    default: match_case = 1'b0;
  endcase
endmodule
"""


def validate(args):
    args.workspace.mkdir(parents=True, exist_ok=True)
    source = args.workspace / "state_shapes.sv"
    source.write_text(RTL)
    zero_spec = args.workspace / "literal_zero_spec.sv"
    # A separate, fully specified golden fixture for the documented native X/Z=0 policy.
    # Leave the casez wildcard pattern unchanged.
    zero_spec.write_text(RTL.replace("2'b0x", "2'b00").replace("1'bx", "1'b0"))
    records = []
    for name, libname in (
        ("sky130", "sky130_fd_sc_hd__tt_025C_1v80.lib"),
        ("gf180", "gf180mcu_fd_sc_mcu7t5v0__tt_025C_3v30.lib"),
    ):
        directory = args.workspace / name
        directory.mkdir(exist_ok=False)
        config = {"directory": str(directory), "tool": "rustsys",
                  "library": str(args.pdks / libname), "corner": "tt",
                  "design": {"top": "state_shapes", "files": [str(source)]},
                  "rustsys": args.rustsys, "rusteda": args.rusteda, "yosys": args.yosys}
        run.save(directory / "config.json", config)
        run.checked([sys.executable, str(run.HERE / "run.py"), "_pipeline",
                     str(directory / "config.json")], directory, "synth", seconds=60, memory_gib=2)
        check_args = [args.rustsys, "--threads", "1", "--progress", "off", "check", "synth",
                      "--top", "state_shapes", "--lib", config["library"], "--sweep-max-candidates", "0",
                      "--budget", "10000", "--netlist"]
        run.checked([*check_args, str(directory / "netlist.v"), "--report", str(directory / "check.json"),
                     str(source)], directory, "check", seconds=60, memory_gib=2)
        native = json.loads((directory / "check.json").read_text())
        if (native["verdict"] != "equivalent" or native["sweep"]["candidates"] != 0
            or native["outputs_checked"] != 11 or native["state_pairs_checked"] != 12):
            raise RuntimeError(f"structural-only sweeping did not prove every final obligation: {name}")
        positive = equiv.prove(config, directory, args.yosys, run.execute, seconds=60, memory_gib=2)
        if positive["verdict"] != "proven":
            raise RuntimeError(f"positive equivalence failed: {name}: {positive}")
        original = (directory / "netlist.v").read_text()
        bad = directory / "mutant"
        bad.mkdir()
        cell, input_pin, output_pin = (("sky130_fd_sc_hd__inv_1", "A", "Y") if name == "sky130"
                                      else ("gf180mcu_fd_sc_mcu7t5v0__inv_1", "I", "ZN"))
        changed, count = re.subn(
            r"(?m)^  assign result\[0\] = ([^;]+);",
            lambda match: f"  {cell} planted_output_error (.{input_pin}({match[1]}), .{output_pin}(result[0]));",
            original)
        if count != 1:
            raise RuntimeError("cannot plant exactly one output polarity error")
        (bad / "netlist.v").write_text(changed)
        mutant_check = run.execute([*check_args, str(bad / "netlist.v"), "--report", str(bad / "check.json"),
                                    str(source)], bad, "check", seconds=60, memory_gib=2)
        if mutant_check["exit_code"] != 3 or json.loads((bad / "check.json").read_text())["verdict"] != "not-equivalent":
            raise RuntimeError(f"RustSys checker did not reject the planted error: {name}")
        negative = equiv.prove(config, bad, args.yosys, run.execute, seconds=60, memory_gib=2)
        log = (bad / "equiv-canonical.stderr").read_text()
        if negative["verdict"] != "unverified" or negative["status"] != "failed" or "unproven" not in log:
            raise RuntimeError(f"planted error was not rejected by the oracle: {name}: {negative}")
        # RTL refinement permits either choice in the undefined branch. The native zero
        # policy and a separate fully specified golden fixture must nevertheless reject 1.
        xbad = directory / "x-branch-mutant"
        xbad.mkdir()
        or_cell, left, right, output = (("sky130_fd_sc_hd__or2_1", "A", "B", "X") if name == "sky130"
                                        else ("gf180mcu_fd_sc_mcu7t5v0__or2_1", "A1", "A2", "Z"))
        changed, count = re.subn(
            r"(?m)^  assign x_choice = ([^;]+);",
            lambda match: (f"  wire planted_x_branch;\n"
                           f"  {cell} planted_x_inverter (.{input_pin}(data[2]), .{output_pin}(planted_x_branch));\n"
                           f"  {or_cell} planted_x_or (.{left}({match[1]}), .{right}(planted_x_branch), .{output}(x_choice));"),
            original)
        if count != 1:
            raise RuntimeError("cannot plant exactly one RTL-X branch error")
        (xbad / "netlist.v").write_text(changed)
        xcheck = run.execute([*check_args, str(xbad / "netlist.v"), "--report", str(xbad / "check.json"),
                              str(source)], xbad, "check", seconds=60, memory_gib=2)
        xproof = equiv.prove(config, xbad, args.yosys, run.execute, seconds=60, memory_gib=2)
        if xcheck["exit_code"] != 3 or xproof["verdict"] != "proven":
            raise RuntimeError(f"RTL-X branch conformance was not checked: {name}")
        spec_config = {**config, "design": {"top": "state_shapes", "files": [str(zero_spec)]}}
        spec_good = directory / "literal-zero-policy"
        spec_good.mkdir()
        spec_positive = equiv.prove({**spec_config, "netlist": str(directory / "netlist.v")},
                                    spec_good, args.yosys, run.execute, seconds=60, memory_gib=2)
        spec_bad = xbad / "literal-zero-policy"
        spec_bad.mkdir()
        spec_negative = equiv.prove({**spec_config, "netlist": str(xbad / "netlist.v")},
                                    spec_bad, args.yosys, run.execute, seconds=60, memory_gib=2)
        spec_error = (spec_bad / "equiv-canonical.stderr").read_text()
        if (spec_positive["verdict"] != "proven" or spec_negative["status"] != "failed"
            or spec_negative["verdict"] != "unverified" or "unproven" not in spec_error):
            raise RuntimeError(f"independent literal-zero policy oracle failed: {name}")
        record = {"library": name, "positive": positive, "planted_error": negative,
                  "rtl_undefined_choice_refinement": xproof,
                  "literal_zero_policy": {"positive": spec_positive, "negative": spec_negative},
                  "native_sweep": native["sweep"]}
        if args.eqy:
            record["eqy"] = {}
            for case, reference, candidate, expected in (
                ("positive", config, directory / "netlist.v", "proven"),
                ("planted_error", config, bad / "netlist.v", "unverified"),
                ("literal_zero_positive", spec_config, directory / "netlist.v", "proven"),
                ("literal_zero_error", spec_config, xbad / "netlist.v", "unverified"),
            ):
                target = directory / ("eqy-" + case)
                target.mkdir()
                proof = lec.prove({**reference, "netlist": str(candidate)}, target,
                                  args.yosys, args.eqy, args.abc, seconds=60, memory_gib=2,
                                  partition_seconds=10)
                if proof["verdict"] != expected:
                    raise RuntimeError(f"EQY {case} had the wrong verdict: {name}: {proof}")
                if expected == "unverified":
                    statuses = list((target / "eqy-work/strategies").glob("*/pdr/*/status"))
                    if not any(p.read_text().startswith("FAIL") for p in statuses):
                        raise RuntimeError(f"EQY did not produce a counterexample for {name}: {case}")
                record["eqy"][case] = proof
        records.append(record)
        print(f"{name}: vector/array state proof passed; planted output error rejected", flush=True)
    run.save(args.workspace / "validation.json", records)


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--workspace", required=True, type=Path)
    p.add_argument("--pdks", required=True, type=Path)
    p.add_argument("--rustsys", default=str(run.ROOT / "target/release/rustsys"))
    p.add_argument("--rusteda", default=str(run.ROOT / "target/release/rusteda"))
    p.add_argument("--yosys", required=True)
    p.add_argument("--eqy", help="Also validate the external EQY/ABC LEC flow")
    p.add_argument("--abc", default="yosys-abc")
    a = p.parse_args()
    a.workspace, a.pdks = a.workspace.resolve(), a.pdks.resolve()
    a.workspace.mkdir(parents=True, exist_ok=True)
    with open(a.workspace / "workspace.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        validate(a)
