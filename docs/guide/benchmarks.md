# Understand the benchmarks

The published benchmark is the Ibex RV32 core with two real IHP SRAMs on IHP
SG13G2 with the ETH EZ130 8T standard-cell library, run end to end through the
native RustEDA flow and compared with LibreLane. Its result is on the
[RustEDA benchmarks page](https://lab-emi.github.io/RustEDA/benchmarks/). This
page describes how RustSys synthesis is run, measured and checked inside that
benchmark.

## Synthesis inside the flow

RustSys runs as an ordinary workspace step of the native flow. `rusteda init`
and `rusteda import` load the RTL with the EZ130 and SRAM Liberty libraries into
a fresh workspace, `rustsys synth --base-rev` maps the design, and
`rusteda export` writes the mapped netlist that the later repair, placement,
routing and signoff stages consume. The two SRAMs are opaque macros during
synthesis. The [agent workflow](agents.md) describes the same commands.

## Measurement boundary

The synthesis step runs under the flow's step time limit and resource guard.
Its time is part of the flow's end-to-end wall clock: following the RustEDA
benchmark method, the clock starts from prepared RTL and ends only when GDS and
the required verification results are complete. The benchmark reports that
whole-flow time. This documentation publishes no separate synthesis-only time.

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

Earlier synthesis-only campaigns on other designs and libraries are no longer
published as benchmark results. Their dated archives of raw timing, quality and
proof receipts remain under
[`verification/benchmarks/open-hardware/results/`](https://github.com/lab-emi/RustSys/tree/main/verification/benchmarks/open-hardware/results/);
the [reproduction guide](https://github.com/lab-emi/RustSys/blob/main/verification/benchmarks/open-hardware/README.md)
lists their pinned inputs and commands. [Verification evidence](../verification/index.md)
describes their proof scopes and the proof invalidation registry, and
[CPU performance and scaling](performance.md) describes the harness method.
