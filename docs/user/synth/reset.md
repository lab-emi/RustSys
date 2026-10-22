# Synthesis after a reset pulse

The default synthesis mode preserves arbitrary initial state. To enable constant-state removal
under an explicit reset contract, pass `--after-reset rst_ni=0` to `rustsys synth` and
`rustsys check synth`. Repeat the option for multiple scalar reset inputs. All specified inputs
must be asserted together before observing the circuit. Hold clocks stable while asserting reset
and until asynchronous state settles, then release reset or resume clocks. States that drive
clocks, latch enables or reset controls are retained. Later inputs remain unrestricted.

The engine removes only flip-flops whose reset establishes a constant that their next-state
functions preserve. It retains scalar aliases with their RTL state names. The checker reads
those aliases independently, validates the reset base cases and proves every next-state
invariant before accepting the output and retained-state comparisons. A run imported with
`check synth --from-run` carries the reset contract with it.

Reports record `after_reset` and removed/checked constants. This mode currently does not infer
synchronous initialization, merge state bits, or remove latches. A reset pulse must actually
occur in use; outputs before it are outside the requested equivalence contract.


For electrical repair at additional process corners, add `--electrical-corner fast.lib` and
`--electrical-corner slow.lib` with `--timing-period`. The libraries must describe the same cells,
pins, functions and areas. Each corner contributes its own capacitance and transition limits and
lookup tables. This repairs electrical limits across corners; verify setup and hold at each corner
with a separate static timing analysis. The report retains unresolved violations if the area budget
is insufficient.
