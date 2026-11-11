#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""part_select_truth.py IVERILOG_BIN_DIR rtl/part_selects.v part-select-icarus-truth.json: record the
Icarus Verilog truth of the part-select fixture.

Outputs are packed least significant first in the order of OUTS; a bit Icarus reports as x or z
(an out-of-range read) is cleared in "known"."""
import hashlib, json, subprocess, sys, tempfile, os
ivl, rtl, out = sys.argv[1:4]
OUTS = [("r_up", 4), ("r_down", 4), ("r_field", 2), ("r_low", 4), ("r_asc", 4), ("r_bits", 5),
        ("r_sum", 4), ("r_wide", 64), ("w_up", 16), ("w_field", 16), ("w_low", 16), ("w_bit", 16),
        ("w_bits", 16), ("w_asc", 16), ("w_two", 16)]
# Every data bit has its own pattern over the five coded vectors (bit p of position + 1).
DATA = [0x0000, 0xFFFF] + [sum(((k + 1) >> p & 1) << k for k in range(16)) for p in range(5)]
W = [0x0, 0xF, 0x6, 0x9, 0x5]
vectors = [(d, i, w) for d in DATA for i in range(8) for w in W]
total = sum(n for _, n in OUTS)
tb = ["module tb; reg [15:0] d; reg [2:0] i; reg [3:0] w;"]
tb += [f"wire [{n - 1}:0] {name};" for name, n in OUTS]
tb.append("part_selects u(" + ", ".join(f".{x}({x})" for x in ["d", "i", "w"] + [o for o, _ in OUTS]) + ");")
tb.append(f"wire [{total - 1}:0] all = {{" + ", ".join(o for o, _ in reversed(OUTS)) + "};")
tb.append("initial begin")
for d, i, w in vectors:
    tb.append(f"d = 16'h{d:04x}; i = {i}; w = 4'h{w:x}; #1 $display(\"%b\", all);")
tb.append("$finish; end endmodule")
with tempfile.TemporaryDirectory() as t:
    open(os.path.join(t, "tb.v"), "w").write("\n".join(tb))
    subprocess.run([os.path.join(ivl, "iverilog"), "-g2005", "-o", os.path.join(t, "a.vvp"),
                    os.path.join(t, "tb.v"), rtl], check=True)
    lines = subprocess.run([os.path.join(ivl, "vvp"), "-n", os.path.join(t, "a.vvp")], check=True,
                           capture_output=True, text=True).stdout.split()
lines = [l for l in lines if len(l) == total and set(l) <= set("01xz")]
assert len(lines) == len(vectors), (len(lines), len(vectors))
rows = []
for (d, i, w), l in zip(vectors, lines):
    value = int("".join("1" if c == "1" else "0" for c in l), 2)
    known = int("".join("1" if c in "01" else "0" for c in l), 2)
    rows.append([d, i, w, f"{value:x}", f"{known:x}"])
sha = lambda p: hashlib.sha256(open(p, "rb").read()).hexdigest()
head = {
    "oracle": "Independent Icarus Verilog simulation; 7 data patterns, all 8 index values, 5 write values",
    "source_sha256": sha(rtl),
    "iverilog_sha256": sha(os.path.join(ivl, "iverilog")),
    "outputs": [n for n, _ in OUTS],
    "widths": [b for _, b in OUTS],
    "packing": "outputs least significant first; [d, i, w, value, known] with hexadecimal value and known mask (0: Icarus reports x)",
}
with open(out, "w") as f:
    f.write("{\n")
    for k, v in head.items():
        f.write(f"  {json.dumps(k)}: {json.dumps(v)},\n")
    f.write('  "vectors": [\n')
    f.write(",\n".join("    " + json.dumps(r) for r in rows))
    f.write("\n  ]\n}\n")
print(len(rows), "vectors", total, "output bits", sum(bin(int(r[4], 16)).count("1") for r in rows), "known bits")
