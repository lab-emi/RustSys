#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Prove the two benchmark latch bindings against Yosys Verilog/Liberty semantics.

Two latch states and two outputs; <1,000 nodes, <128 MiB expected. The child has a
1 GiB address-space limit and 30 s process-group deadline. No RustSys oracle is used.
"""
import argparse
from pathlib import Path
import run

GOLD = """module latch_pair(input enable, data, output reg qp, qn);
  always @* if (enable) qp = data;
  always @* if (!enable) qn = data;
endmodule
"""


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--workspace", type=Path, required=True)
    p.add_argument("--pdks", type=Path, required=True)
    p.add_argument("--yosys", required=True)
    args = p.parse_args()
    for name, library in (
        ("sky130", "sky130_fd_sc_hd__tt_025C_1v80.lib"),
        ("gf180", "gf180mcu_fd_sc_mcu7t5v0__tt_025C_3v30.lib"),
    ):
        directory = args.workspace.resolve() / name
        directory.mkdir(parents=True, exist_ok=True)
        source = directory / "latches.v"
        source.write_text(GOLD)
        script = directory / "check.ys"
        script.write_text(
            f"read_verilog {run.quote(source)}\nprep -top latch_pair\nasync2sync\n"
            "rename latch_pair gold\ndesign -stash gold\n"
            f"read_verilog {run.quote(source)}\nsynth -top latch_pair -noabc\n"
            f"techmap -map {run.quote(run.HERE / ('latches-' + name + '.v'))}\ntechmap\n"
            f"read_liberty -ignore_miss_func -ignore_miss_dir {run.quote(args.pdks.resolve() / library)}\n"
            "hierarchy -check -top latch_pair\nflatten\nproc\nopt\nasync2sync\n"
            "rename latch_pair gate\ndesign -copy-from gold gold\n"
            "equiv_make gold gate equiv\nhierarchy -top equiv\nequiv_simple\nequiv_induct -seq 4\nequiv_status -assert\n"
        )
        r = run.checked([args.yosys, "-Q", "-T", "-s", str(script)], directory,
                        "check", seconds=30, memory_gib=1)
        log = (directory / "check.stdout").read_text()
        if "Equivalence successfully proven!" not in log:
            raise RuntimeError(f"no completed proof for {name}")
        print(f"{name}: latch bindings proven; RSS {r['peak_rss_kib']} KiB")


if __name__ == "__main__":
    main()
