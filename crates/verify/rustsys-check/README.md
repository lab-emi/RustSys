# rustsys-check

The independent synthesis checker (`docs/specs/check/synth.md`): it decides whether a gate-level
netlist, such as the output of `rustsys synth`, is equivalent to its RTL. It shares no code with
the synthesis engine; of the workspace crates it uses only `rusteda-formats` and `rusteda-core`.
It interprets the RTL with its own code, evaluates the netlist from the Liberty descriptions with
its own function parser, and proves equivalence with its own SAT solver. A netlist construct
outside its model is rejected with a coded error, and a proof that runs out of budget gives the
verdict `undecided`, never `equivalent` or `error`.

Entry point: `check` (or `check_with` for an observed, cancellable run) in `lib.rs`. The Liberty
attributes that the library model does not keep, such as `driver_type`, come from the library
text through `liberty::unread`; a caller that reads the libraries itself passes them in
`CheckOptions::unread_liberty`, as `rustsys check synth` does.
