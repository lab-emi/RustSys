# Understand the benchmarks

The published benchmark is the Ibex RV32 core with two real IHP SRAMs on IHP
SG13G2 with the ETH EZ130 8T standard-cell library, run end to end through the
native RustEDA flow and compared with LibreLane. Its result is on the
[RustEDA benchmarks page](https://lab-emi.github.io/RustEDA/benchmarks/). This
page describes how RustSys synthesis is run, measured and checked inside that
benchmark, and points to the RustSys-vs-Yosys module head-to-head that the same
page publishes for the 2026-11-02 host campaign.

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

The current synthesis comparison is the host campaign's RustSys-vs-Yosys
module head-to-head above, published as part of the RustEDA whole-flow
benchmark page.
Earlier synthesis-only campaigns on other designs and libraries are no longer
published as benchmark results. Their dated archives of raw timing, quality and
proof receipts remain under
[`verification/benchmarks/open-hardware/results/`](https://github.com/lab-emi/RustSys/tree/main/verification/benchmarks/open-hardware/results/);
the [reproduction guide](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/README.md)
lists their pinned inputs and commands. [Verification evidence](../verification/index.md)
describes their proof scopes and the proof invalidation registry, and
[CPU performance and scaling](performance.md) describes the harness method.
