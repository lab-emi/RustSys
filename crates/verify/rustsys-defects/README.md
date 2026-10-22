# rustsys-defects

Plants known faults into valid gate-level netlists so that the independent synthesis checker can
be tested against every fault class (CHECK-REQ-0070).

- `read_netlist` parses a netlist of the structural subset into an editable module, which
  `rusteda_formats::verilog::write::write_netlist` writes back.
- `plant(netlist, library, class, seed)` plants one defect of a class at a site chosen by the
  seed, or returns `None` when the netlist has no suitable site; the cell functions come from the
  Liberty library.
- `DefectClass::ALL` lists the eleven classes:

| Class | Defect |
|---|---|
| `CellSubstitution` | a combinational cell replaced by a usable cell with the same pins and another function |
| `PinSwap` | the nets on two inputs of an asymmetric cell exchanged |
| `InvertedNet` | an inverter inserted in front of a combinational cell input |
| `StuckAt0`, `StuckAt1` | a combinational cell input tied to a constant |
| `Rewire` | a combinational cell input moved to the output of an earlier combinational cell |
| `RemovedInverter` | an inverter removed, its readers connected to its input |
| `ClockInversion` | an inverter inserted in front of a sequential cell's clock pin |
| `AsyncPolarity` | an inverter inserted in front of an asynchronous clear or preset pin |
| `AsyncValue` | a clear flip-flop exchanged for a preset flip-flop with the same clock, data and control polarity, or vice versa |
| `SwappedStateNames` | the names of two state wires exchanged |

The acceptance test that runs the checker on every class is the suite `defects`
(`crates/apps/rustsys-cli/tests/defects.rs`).
