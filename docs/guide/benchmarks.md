# Understand the benchmarks

The published benchmark is the Ibex RV32 core with two real IHP SRAMs on IHP
SG13G2 with the ETH EZ130 8T standard-cell library, run end to end through the
native RustEDA flow and compared with LibreLane. Its result is on the
[RustEDA benchmarks page](https://lab-emi.github.io/RustEDA/benchmarks/). This
page describes how RustSys synthesis is run, measured and checked inside that
benchmark, and points to the RustSys-vs-Yosys module head-to-heads that the same
page publishes for the 2026-11-02, 2026-11-05 and 2026-11-13 host campaigns.

## Synthesis inside the flow

RustSys runs as an ordinary workspace step of the native flow. `rusteda init`
and `rusteda import` load the RTL with the EZ130 and SRAM Liberty libraries into
a fresh workspace, `rustsys synth --base-rev` maps the design, and
`rusteda export` writes the mapped netlist that the later repair, placement,
routing and signoff stages consume. The two SRAMs are opaque macros during
synthesis. The [agent workflow](agents.md) describes the same commands.

## Measurement boundary

The synthesis step runs under the flow's step time limit and resource guard.
Its time is part of the flow's end-to-end wall clock, whose scope is stated per
campaign on the RustEDA benchmarks page. In the 2026-10-30 matched comparison
the clock starts from frozen raw SystemVerilog, includes preparation, and ends
only when GDS and the required verification results are complete; the R20/R21
runs time the whole worker. The 2026-11-02 host campaign
is timed differently: its end-to-end time is the sum of separately timed parts,
namely the sv2v preparation of the RTL, the producer run, which stopped at the
`timing_accuracy` gate, and the shared gates the producer did not reach. The
[host campaign](https://lab-emi.github.io/RustEDA/benchmarks/#ibex-host-campaign-2026-11-02)
states that scope. The benchmark reports whole-flow time, and this
documentation does not copy synthesis times. The synthesis head-to-head below
is measured outside the flow and published on the RustEDA benchmarks page.

## Synthesis head-to-head · 2026-11-13

The [2026-11-13 host campaign](https://lab-emi.github.io/RustEDA/benchmarks/#ibex-host-campaign-2026-11-13)
repeated the comparison below on another 4-vCPU host, with the same task and
tools and RustSys [`86187e6`](https://github.com/lab-emi/RustSys/commit/86187e69c3111ad79ae4b651ad53d652b7609867)
built with Rust 1.100.0.
**RustSys was faster, used less memory and produced the smaller and shallower
netlist:**

| | RustSys | Yosys 0.70 + ABC, LibreLane script |
| --- | ---: | ---: |
| Median wall, workspace sequence (fastest) | 3.07 s (3.06 s) | 103.91 s (102.47 s) |
| Median wall, direct mode (fastest) | 2.19 s (2.05 s) | — |
| Worst peak RSS, sequence / direct | 116,264 / 112,660 KiB | 765,624 KiB |
| Standard-cell area (`yosys stat -liberty`) | 145,226.592 µm² (-1.79 %) | 147,866.947 µm² |
| Area including both SRAM macros | 425,591.9808 µm² | 428,232.336 µm² |
| Logic depth in cells (excluding buffers) | 36 (36) | 40 (40) |
| Cells: total / sequential / buffer and inverter / other / macro | 11,433 / 1,998 / 950 / 8,483 / 2 | 12,605 / 1,905 / 186 / 10,512 / 2 |

Yosys took 33.8× RustSys's median wall time (sequence) and 47.4× (direct). The
native flow's RustSys netlist is `proven` by the flow's synthesis gate;
LibreLane's synthesis netlist is `unverified`. The flow's synthesis netlist is
byte-identical to 2026-11-05's and the head-to-head QoR is unchanged; the times
differ from 2026-11-05's (sequence 3.07 s against 3.81 s, Yosys 103.91 s against
135.04 s) on a different host, on which every LibreLane stage also took less
time (floorplan level), so the comparison is history, not a speedup.
[RustSys vs Yosys · 2026-11-13](https://lab-emi.github.io/RustEDA/benchmarks/#rustsys-vs-yosys-2026-11-13)

## Synthesis head-to-head · 2026-11-05

The [2026-11-05 host campaign](https://lab-emi.github.io/RustEDA/benchmarks/#ibex-host-campaign-2026-11-05)
repeated the comparison below with RustSys [`86e59d8`](https://github.com/lab-emi/RustSys/commit/86e59d816e6ded82333ccc502bb37961c3bb1a9f) on another 4-vCPU host.
**RustSys was faster, used less memory and produced the smaller and shallower
netlist:**

| | RustSys | Yosys 0.70 + ABC, LibreLane script |
| --- | ---: | ---: |
| Median wall, workspace sequence (fastest) | 3.81 s (3.62 s) | 135.04 s (126.82 s) |
| Median wall, direct mode (fastest) | 3.04 s (2.75 s) | — |
| Worst peak RSS, sequence / direct | 116,200 / 115,052 KiB | 766,036 KiB |
| Standard-cell area (`yosys stat -liberty`) | 145,226.592 µm² | 147,866.947 µm² |
| Logic depth in cells, excluding buffers | 36 (36) | 40 (40) |
| Cells: total / sequential / buffer and inverter | 11,433 / 1,998 / 950 | 12,605 / 1,905 / 186 |

Yosys took 35.4× RustSys's median wall time (sequence) and 44.4× (direct). The
native flow's RustSys netlist is `proven` by the flow's synthesis gate;
LibreLane's synthesis netlist is `unverified`.

## Synthesis head-to-head · 2026-11-02

The [Ibex host campaign](https://lab-emi.github.io/RustEDA/benchmarks/#ibex-host-campaign-2026-11-02)
on a 4-vCPU cloud host includes, among its module head-to-heads, a
[RustSys-vs-Yosys comparison](https://lab-emi.github.io/RustEDA/benchmarks/#rustsys-vs-yosys).
Both contenders map the same prepared Ibex RTL (sv2v output) onto the same
EZ130 TT and SRAM Liberty. RustSys runs the four-command workspace sequence
described above at the recipe's 2 threads, and separately its single-process
direct mode (`--threads 0`, all 4 CPUs); Yosys 0.70 runs LibreLane 3.0.17's own synthesis script, with ABC.
Each contender has one warm-up and three measured runs, interleaved. For both
netlists, standard-cell area comes from `yosys stat -liberty`, and logic depth
is the longest combinational cell path between registers, macros and ports,
computed by the harness on Yosys's JSON netlist.

In that comparison RustSys has the shorter median wall time in both modes and
the lower peak RSS (largest process, GNU time). The Yosys + ABC netlist is
smaller in standard-cell area and has fewer logic levels, also when buffer,
inverter and delay cells are excluded; the RustSys netlist has slightly fewer
cells. The equivalence verdicts come from the flows' synthesis-equivalence
gates, not from the head-to-head netlists: the native flow's RustSys netlist is
`proven`, and LibreLane's synthesis netlist is `unverified`, which is not evidence of
non-equivalence. The head-to-head RustSys netlists differ in bytes from the
proven one and were not checked separately. The RustEDA page gives
the measured values, their scope and the host's deviations; quotients there
apply to that task and host only.

## Equivalence

The flow's synthesis equivalence gate proves the synthesized netlist equivalent
to the original RTL; its final equivalence gate does the same for the final
netlist. Each gate binds the hashes of the RTL and of the netlist it checks, and
a gate that is not proven fails the run. A successful synthesis exit status is
not evidence of equivalence.

The proof uses two-valued RTL event semantics: ordinary X/Z data is completed
to zero, original and implementation initial states stay arbitrary, and no
reset or input assumption is added. It does not establish four-state refinement
or the equivalence of the SRAM macro internals; the flow exercises both SRAMs
separately in simulation against the vendor models. The
[checking guide](checking.md) explains this semantic model.

## Earlier synthesis-only campaigns

The current synthesis comparison is the host campaigns' RustSys-vs-Yosys
module head-to-heads above, published as part of the RustEDA whole-flow
benchmark page.
Earlier synthesis-only campaigns on other designs and libraries are no longer
published as benchmark results. Their dated archives of raw timing, quality and
proof receipts remain under
[`verification/benchmarks/open-hardware/results/`](https://github.com/lab-emi/RustSys/tree/main/verification/benchmarks/open-hardware/results/);
the [reproduction guide](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/README.md)
lists their pinned inputs and commands. [Verification evidence](../verification/index.md)
describes their proof scopes and the proof invalidation registry, and
[CPU performance and scaling](performance.md) describes the harness method.
