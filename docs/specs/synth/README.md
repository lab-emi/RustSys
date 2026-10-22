# RustSys specification

RustSys (`rustsys`, command `rustsys synth`) translates synthesizable SystemVerilog RTL
into a flat gate-level netlist of standard cells described by Liberty libraries.

| Document | Requirement range | Content |
|---|---|---|
| [frontend.md](frontend.md) | SYNTH-REQ-01xx | accepted language subset, elaboration |
| [semantics.md](semantics.md) | SYNTH-REQ-02xx | value system, expression and statement semantics |
| [sequential.md](sequential.md) | SYNTH-REQ-03xx | registers and latches |
| [optimization.md](optimization.md) | SYNTH-REQ-04xx | internal logic representation and optimization |
| [mapping.md](mapping.md) | SYNTH-REQ-05xx | standard-cell selection and mapping |
| [netlist.md](netlist.md) | SYNTH-REQ-06xx | output netlist contract |
| [cli.md](cli.md) | SYNTH-REQ-07xx | command line, diagnostics, reports |
| [verification.md](verification.md) | SYNTH-REQ-08xx | verification plan |
| [inputs.md](inputs.md) | SYNTH-REQ-0900 to 0949 | BLIF, AIGER and gate-level netlist sources |

Related specifications: `docs/specs/verilog/` (VERILOG-REQ), `docs/specs/liberty/`
(LIBERTY-REQ), `docs/specs/blif/` (BLIF-REQ), `docs/specs/aiger/` (AIGER-REQ), `docs/specs/check/synth.md` (CHECK-REQ), `docs/specs/compute/` (COMPUTE-REQ).

Normative words follow RFC 2119. "The standard" means IEEE 1800-2017 [@ieee1800_2017] unless
another document is named. Citation keys refer to `docs/references.bib`.
