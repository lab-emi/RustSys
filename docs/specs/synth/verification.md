# RustSys verification plan

**SYNTH-REQ-0801** Every netlist produced by the suites is checked by the independent synthesis
checker (`docs/specs/check/synth.md`), which proves equivalence of the netlist and the RTL under
SYNTH-REQ-0201 and the state correspondence of SYNTH-REQ-0603.

**SYNTH-REQ-0802** Suites (`verification/suites/synth/`):
- `corpus`: hand-written RTL modules typical of digital designs (counters, state machines, FIFOs,
  arithmetic units, arbiters, encoders, CRC and LFSR generators, shifters, register files,
  pipelines using packages, structures, enumerations and generate constructs), synthesized with
  each synthetic library of `rustsys-fixtures`;
- `random`: seeded random RTL modules produced by `rustsys-fixtures` covering operators, widths,
  signedness, procedural control flow and registers;
- `known-answer`: combinational modules with at most 16 inputs whose netlists are compared with
  exhaustive truth tables computed by an `f64`-free scalar evaluator of the RTL;
- `metamorphic`: resynthesis of a synthesized netlist is equivalent to it (idempotence up to
  equivalence); renaming a module port does not change the cell counts; reordering independent
  module items does not change the netlist function;
- `defects`: planted defects in synthesized netlists (`rustsys-defects`) must be reported by the
  checker, one case per defect class.

**SYNTH-REQ-0803** Unit tests cover every requirement of this specification that is observable at
the crate API, and cite the requirement IDs.
