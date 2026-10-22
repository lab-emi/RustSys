#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Independent semantic controls for the inductive transition proof.

All fixtures have fewer than 16 state bits and 100 logic cells. One external
worker at a time; expected RSS below 256 MiB, hard child limit 1 GiB, 60 s/step.
Use an outer 2 GiB cgroup and wall-clock guard as for the other validators.
"""
import argparse
from pathlib import Path

import run
import transition
import rtl_semantics

FLOP = "module t(input clk,rst,en,d,output reg q); always @(posedge clk or negedge rst) if(!rst)q<=0;else if(en)q<=d; endmodule\n"
LATCH = "module t(input clk,rst,en,d,output q); reg e;reg s;always @* if(!clk)e=en|s;wire c=clk&e;always @(posedge c or negedge rst)if(!rst)s<=0;else s<=d;assign q=s;endmodule\n"
INIT = "module t(input clk,d,output reg q);initial q=0;always @(posedge clk)q<=d;endmodule\n"
PRIORITY = "module t(input a,b,d,output reg q);always @* begin q=1;(* full_case,parallel_case *)case(1'b1)a:q=0;b:q=d;default:q=1;endcase end endmodule\n"
INCOMPLETE_CASE = "module t(input [1:0]s,input d,output reg q);always @* (*full_case*)case(s)0:q=0;1:q=d;endcase endmodule"
CASES = [
    ("word_arithmetic", "module t(input [5:0]a,b,output [6:0]q);assign q={1'b0,a}+{1'b0,b};endmodule", "module t(input [5:0]a,b,output [6:0]q);assign q={1'b0,b}+{1'b0,a};endmodule", True),
    ("word_arithmetic_error", "module t(input [5:0]a,b,output [6:0]q);assign q={1'b0,a}+{1'b0,b};endmodule", "module t(input [5:0]a,b,output [6:0]q);assign q={1'b0,a}-{1'b0,b};endmodule", False),
    ("unpaired_const_unused", "module t(input clk,d,output q);(*keep*)reg hidden;always @(posedge clk)hidden<=d;assign q=d;endmodule", "module t(input clk,d,output q);(*keep*)wire hidden=0;assign q=d;endmodule", True),
    ("unpaired_const_observable_error", "module t(input clk,d,output q);(*keep*)reg hidden;always @(posedge clk)hidden<=d;assign q=hidden;endmodule", "module t(input clk,d,output q);(*keep*)wire hidden=0;assign q=0;endmodule", False),
    ("async_visible_state", "module t(input clk,rst,d,output reg q);reg a;always @(posedge clk or negedge rst)if(!rst)a<=0;else a<=d;always @(posedge clk)q<=a;endmodule", "module t(input clk,rst,d,output reg q);reg a;always @(posedge clk or negedge rst)if(!rst)a<=0;else a<=d;always @(posedge clk)q<=rst?a:0;endmodule", True),
    ("async_visible_state_error", "module t(input clk,rst,d,output reg q);reg a;always @(posedge clk or negedge rst)if(!rst)a<=0;else a<=d;always @(posedge clk)q<=a;endmodule", "module t(input clk,rst,d,output reg q);reg a;always @(posedge clk or negedge rst)if(!rst)a<=0;else a<=d;always @(posedge clk)q<=rst?a:1;endmodule", False),
    ("undriven_reference", "module t(input d,output q);wire unused;assign q=unused;endmodule", "module t(input d,output q);assign q=d;endmodule", True),
    ("undriven_implementation", "module t(input d,output q);assign q=d;endmodule", "module t(input d,output q);wire unused;assign q=unused;endmodule", False),
    ("case_priority", PRIORITY, "module t(input a,b,d,output q);assign q=a?0:b?d:1;endmodule", True),
    ("case_priority_error", PRIORITY, "module t(input a,b,d,output q);assign q=b?d:a?0:1;endmodule", False),
    ("incomplete_case_latch", INCOMPLETE_CASE, INCOMPLETE_CASE.replace('(*full_case*)', ''), True),
    ("incomplete_case_latch_error", INCOMPLETE_CASE, "module t(input [1:0]s,input d,output q);assign q=s==0?0:d;endmodule", False),
    ("same", FLOP, FLOP, True),
    ("data_error", FLOP, FLOP.replace("q<=d", "q<=~d"), False),
    ("clock_error", FLOP, FLOP.replace("posedge clk", "negedge clk"), False),
    ("reset_error", FLOP, FLOP.replace("q<=0", "q<=1"), False),
    ("enable_error", FLOP, FLOP.replace("if(en)q<=d", "q<=d"), False),
    ("clock_gate", LATCH, LATCH, True),
    ("clock_bypass", LATCH, LATCH.replace("wire c=clk&e", "wire c=clk"), False),
    ("initial_same", INIT, INIT, True),
    ("initial_error", INIT, INIT.replace("initial q=0", "initial q=1"), False),
    ("initial_weakened", INIT, INIT.replace("initial q=0;", ""), False),
    ("initial_refinement", INIT.replace("initial q=0;", ""), INIT, True),
    ("sync_reset_error", FLOP, FLOP.replace("posedge clk or negedge rst", "posedge clk"), False),
    ("constant_dc", "module t(input a,output q);assign q=1'bx;endmodule", "module t(input a,output q);assign q=a;endmodule", True),
    ("gate_unknown", "module t(input a,output q);assign q=0;endmodule", "module t(input a,output q);assign q=1'bx;endmodule", False),
    ("partial_dc", "module t(input a,b,output [1:0] q);assign q={1'bx,a};endmodule", "module t(input a,b,output [1:0] q);assign q={b,a};endmodule", True),
    ("partial_dc_error", "module t(input a,b,output [1:0] q);assign q={1'bx,a};endmodule", "module t(input a,b,output [1:0] q);assign q={b,~a};endmodule", False),
    ("dynamic_dc", "module t(input en,d,output q);assign q=en?1'bx:d;endmodule", "module t(input en,d,output q);assign q=en|d;endmodule", True),
    ("dynamic_dc_error", "module t(input en,d,output q);assign q=en?1'bx:d;endmodule", "module t(input en,d,output q);assign q=en|~d;endmodule", False),
    ("masked_dc_error", "module t(input d,output q);assign q=d & 1'bx;endmodule", "module t(input d,output q);assign q=1;endmodule", False),
    ("dynamic_next_dc", "module t(input clk,en,d,output reg q);always @(posedge clk)q<=en?1'bx:d;endmodule", "module t(input clk,en,d,output reg q);always @(posedge clk)q<=en|d;endmodule", True),
    ("dynamic_next_dc_error", "module t(input clk,en,d,output reg q);always @(posedge clk)q<=en?1'bx:d;endmodule", "module t(input clk,en,d,output reg q);always @(posedge clk)q<=en|~d;endmodule", False),
    ("anonymous_scope", "module t(input clk,d,output q);generate if(1)begin:genblk3 reg state;always @(posedge clk)state<=d;assign q=~state;end endgenerate endmodule", "module t(input clk,d,output q);reg state;always @(posedge clk)state<=d;assign q=~state;endmodule", True),
    ("anonymous_scope_error", "module t(input clk,d,output q);generate if(1)begin:genblk3 reg state;always @(posedge clk)state<=d;assign q=~state;end endgenerate endmodule", "module t(input clk,d,output q);reg state;always @(posedge clk)state<=~d;assign q=~state;endmodule", False),
    ("duplicate_enabled_state", "module t(input clk,en,d,output y,z);reg a,b;always @(posedge clk)if(en)begin a<=d;b<=d;end assign y=a^b;assign z=a;endmodule", "module t(input clk,en,d,output y,z);reg a;always @(posedge clk)if(en)a<=d;assign y=0;assign z=a;endmodule", True),
    ("duplicate_enabled_state_error", "module t(input clk,en,d,output y,z);reg a,b;always @(posedge clk)if(en)begin a<=d;b<=d;end assign y=a^b;assign z=a;endmodule", "module t(input clk,en,d,output y,z);reg a;always @(posedge clk)if(en)a<=~d;assign y=0;assign z=a;endmodule", False),
    ("duplicate_initial_conflict", "module t(input clk,en,d,output y,z);reg a,b;initial begin a=0;b=1;end always @(posedge clk)if(en)begin a<=d;b<=d;end assign y=a^b;assign z=a;endmodule", "module t(input clk,en,d,output y,z);reg a;initial a=0;always @(posedge clk)if(en)a<=d;assign y=0;assign z=a;endmodule", False),
    ("inactive_clock_data", "module t(input clk,d,output reg q);always @(posedge clk)q<=d;endmodule", "module t(input clk,d,output reg q);always @(posedge clk)q<=clk?d:~d;endmodule", True),
    ("active_clock_data_error", "module t(input clk,d,output reg q);always @(posedge clk)q<=d;endmodule", "module t(input clk,d,output reg q);always @(posedge clk)q<=clk?~d:d;endmodule", False),
    ("closed_latch_data", "module t(input en,d,output reg q);always @* if(en)q=d;endmodule", "module t(input en,d,output reg q);always @* if(en)q=en?d:~d;endmodule", True),
    ("reset_data_irrelevant", "module t(input clk,rst,d,output reg q);always @(posedge clk or negedge rst)if(!rst)q<=0;else q<=d;endmodule", "module t(input clk,rst,d,output reg q);always @(posedge clk or negedge rst)if(!rst)q<=0;else q<=rst?d:~d;endmodule", True),
    ("constant_event_invariant", "module t(input clk,d,output reg q);initial q=0;always @(posedge clk)q<=clk?0:1;endmodule", "module t(input clk,d,output q);assign q=0;endmodule", True),
    ("complemented_flop", "module t(input clk,d,output reg q);always @(posedge clk)q<=d;endmodule", "module t(input clk,d,output q);reg nq;always @(posedge clk)nq<=~d;assign q=~nq;endmodule", True),
    ("complemented_flop_error", "module t(input clk,d,output reg q);always @(posedge clk)q<=d;endmodule", "module t(input clk,d,output q);reg nq;always @(posedge clk)nq<=d;assign q=~nq;endmodule", False),
    ("complemented_reset", "module t(input clk,rst,d,output reg q);always @(posedge clk or negedge rst)if(!rst)q<=0;else q<=d;endmodule", "module t(input clk,rst,d,output q);reg nq;always @(posedge clk or negedge rst)if(!rst)nq<=1;else nq<=~d;assign q=~nq;endmodule", True),
    ("complemented_reset_error", "module t(input clk,rst,d,output reg q);always @(posedge clk or negedge rst)if(!rst)q<=0;else q<=d;endmodule", "module t(input clk,rst,d,output q);reg nq;always @(posedge clk or negedge rst)if(!rst)nq<=0;else nq<=~d;assign q=~nq;endmodule", False),
    ("complemented_latch", "module t(input en,d,output reg q);always @*if(en)q=d;endmodule", "module t(input en,d,output q);reg nq;always @*if(en)nq=~d;assign q=~nq;endmodule", True),
    ("complemented_initial_error", "module t(input clk,d,output reg q);initial q=0;always @(posedge clk)q<=d;endmodule", "module t(input clk,d,output q);reg nq;initial nq=0;always @(posedge clk)nq<=~d;assign q=~nq;endmodule", False),
]


def validate(args):
    args.workspace.mkdir(parents=True, exist_ok=False)
    results = []
    cases = list(CASES)
    if args.partial_gate:
        cases += [
            ('same_dynamic_unknown', "module t(input en,d,output q);assign q=en?1'bx:d;endmodule", "module t(input en,d,output q);assign q=en?1'bx:d;endmodule", True),
            ('target_loses_definedness', "module t(input en,d,output q);assign q=d;endmodule", "module t(input en,d,output q);assign q=en?1'bx:d;endmodule", False),
            ('target_vector_loses_definedness', "module t(input d,output [3:0]q);assign q={d,3'b000};endmodule", "module t(input d,output [3:0]q);assign q={d,1'bx,2'b00};endmodule", False),
            ('target_refines_unknown', "module t(input en,d,output q);assign q=en?1'bx:d;endmodule", "module t(input en,d,output q);assign q=en?1'b0:d;endmodule", True),
            ('unknown_state_update', "module t(input clk,en,d,output reg q);always @(posedge clk)q<=en?1'bx:d;endmodule", "module t(input clk,en,d,output reg q);always @(posedge clk)q<=en?1'bx:d;endmodule", True),
        ]
    for name, gold, gate, expected in cases:
        directory = args.workspace / name
        directory.mkdir()
        script = []
        for side, text in (("gold", gold), ("gate", gate)):
            path = directory / f"{side}.v"
            path.write_text(text)
            if side == "gold":
                normalized_verilog = directory / "priority.v"
                run.save(directory / "case-source.json", rtl_semantics.verilog_priority(path, normalized_verilog))
                raw, normalized = directory / "case-promises.il", directory / "priority.il"
                pre = directory / "pre-case.ys"
                pre.write_text(f'read_verilog "{normalized_verilog}"\nhierarchy -top t\nwrite_rtlil "{raw}"\n')
                step = run.execute([args.yosys, "-Q", "-T", "-s", str(pre)], directory,
                                   "pre-case", seconds=60, memory_gib=1)
                if step["status"] != "ok":
                    raise RuntimeError("case fixture frontend failed")
                run.save(directory / "case-normalization.json", rtl_semantics.priority_cases(raw, normalized))
                read = f'read_rtlil "{normalized}"'
            else:
                read = f'read_verilog "{path}"'
            script += [read, "hierarchy -top t", "proc", "flatten", "opt_clean",
                       f'write_rtlil "{directory / (side + ".il")}"', "design -reset"]
        (directory / "prepare.ys").write_text("\n".join(script) + "\n")
        prepared = run.execute([args.yosys, "-Q", "-T", "-s", str(directory / "prepare.ys")],
                               directory, "prepare", seconds=60, memory_gib=1)
        if prepared["status"] != "ok":
            raise RuntimeError("fixture preparation failed")
        limits = transition.Limits(max_cells=1000, max_bits=16384, max_state=128,
                                   max_connections=65536, max_json_bytes=64 * 1024**2,
                                   constant_exclusions=('hidden',) if name.startswith('unpaired_const') else ())
        result, diagnostic = None, None
        try:
            transition.prepare(directory, directory / "transition", "t", args.yosys, 60, 1, limits)
            result = transition.compare(directory / "transition", directory / "cec", "t",
                                        args.yosys, args.abc, 60, 1, limits, partial_gate=args.partial_gate)
            proven = result["equivalent"]
        except ValueError as error:
            proven, diagnostic = False, str(error)
        if proven != expected:
            raise RuntimeError(f"unexpected transition result for {name}: {diagnostic or result}")
        results.append({"case": name, "expected": expected, "proven": proven,
                        "diagnostic": diagnostic, "comparison": result})
        print(name, "proven" if proven else "rejected", flush=True)
    run.save(args.workspace / "validation.json", results)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--yosys", required=True)
    parser.add_argument("--abc", required=True)
    parser.add_argument('--partial-gate', action='store_true')
    args = parser.parse_args()
    args.workspace = args.workspace.resolve()
    validate(args)
