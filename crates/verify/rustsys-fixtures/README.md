# rustsys-fixtures

Generators of synthetic test inputs. Everything under `verification/fixtures/synthetic/` is
produced from this crate (`cargo xtask fixtures`).

- `liberty`: toy cell libraries with our own cell names, functions and timing values
  (`wt_rich`, `wt_reset_only`, `wt_minimal`), and through `liberty_full` a library written in the
  full style of production Liberty files (`wt_full`).
- `rtl`: the corpus of hand-written SystemVerilog modules typical of digital designs (`CORPUS`).
- `random`: seeded random combinational modules with their own evaluator, and random sequential
  modules (`random_comb`, `random_seq`).
