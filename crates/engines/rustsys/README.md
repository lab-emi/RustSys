# rustsys

The RustSys synthesis engine: logic synthesis of SystemVerilog RTL into standard-cell netlists
(`docs/specs/synth/`).

Pipeline: `elab` (elaboration and symbolic execution into a bit-level And-Inverter Graph with
state elements) → `opt` (sweep, balance, rewrite) → `map` (cut-based standard-cell mapping,
sequential cell selection) → `emit` (structural Verilog per the netlist contract), optionally
followed by `electrical` (load-aware buffering and sizing).

The mapper's cut passes run on the CPU or, with the Cargo features `cuda` and `metal`, on a GPU
through `rusteda-compute` (SYNTH-REQ-0516, COMPUTE-REQ-0101 to 0106).

Entry point: `synthesize` in `lib.rs`; front ends use the items re-exported by `view`.
