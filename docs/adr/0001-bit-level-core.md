# ADR 0001: RustSys elaborates directly to a bit-level And-Inverter Graph

- Status: proposed
- Date: 2026-09-27

## Context
RustSys must turn SystemVerilog RTL into standard-cell netlists whose correctness can be proven
by an independent checker. The first version must cover typical synthesizable RTL; datapath
optimizations and GPU acceleration follow later.

## Decision
- Elaboration flattens the hierarchy and interprets procedures symbolically, producing bit vectors
  of And-Inverter Graph literals directly (no word-level intermediate form in this version).
- The AIG is stored as structure-of-arrays with topologically ordered ids (SYNTH-REQ-0401), so
  cut enumeration and simulation can later run as device kernels without a data-layout change.
- Optimization and mapping run on the CPU in this version. GPU kernels are added behind the
  `rustsys` feature flags once CPU results are verified, and must reproduce CPU results
  bit for bit.
- State elements are preserved one-to-one (SYNTH-REQ-0308) and named per SYNTH-REQ-0603, so that
  equivalence reduces to combinational checks with a known state correspondence.

## Consequences
- Word-level simplifications (for example operator sharing) are not available in this version.
- Very wide arithmetic creates large graphs; this is acceptable for the targeted RTL.
- The checker can match state by name, which keeps it simple (CHECK-REQ-0031).
