# rustsys-cli

The `rustsys` program (`docs/specs/cli/README.md`, CLI-REQ): a library (`rustsys_cli`) behind a
thin binary.

- `rustsys synth`: synthesis and standard-cell mapping of SystemVerilog RTL
  (`docs/specs/synth/cli.md`);
- `rustsys check synth`: the independent synthesis checker (`docs/specs/check/synth.md`);
- `rustsys show`: sections of a recorded run or a report file;
- `rustsys completions`: shell completion scripts;
- workspace-first (`-w <WS>`): `synth`, `check`, `report`, and the post-mapping steps
  `opt-timing` and `resize`, whose `path<N>` and `ep<N>` targets are read from the timing table
  of the base revision and whose timing target comes from its constraints table
  (CLI-REQ-0200 to CLI-REQ-0213).

Results go to standard output, coded messages and progress to standard error. The default build
includes the `cuda` runtime of the GPU mapping backend (`rusteda-compute`); there is no Metal
runtime yet.

The verification suites under `tests/` run the synthesis engine and the checker on the synthetic
designs and libraries of `rustsys-fixtures` and `rustsys-defects`. `benches/backends.rs` times
synthesis with every mapping backend of the build (`RUSTSYS_BENCH_K`, `RUSTSYS_BENCH_REPS`).
