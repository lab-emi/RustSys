# Zynq-7000 / Ibex acceptance

`rustsys synth-fpga` maps workspace RTL natively to LUT1–LUT6, FDRE/FDCE/FDPE,
LDCE/LDPE, BUFG and constant primitives. It invokes no external synthesizer and
keeps the existing ASIC synthesis path separate. The first target is
**xc7z020clg400-1**, an out-of-context Ibex core with a 100 ns clock period.
Other `xc7z` devices are accepted for Vivado to validate package/speed grade;
Zynq UltraScale+ is rejected explicitly. PS integration, board pins and bitstream
generation are outside this initial scope.

## Current status

Native mapping, independent Icarus RTL/netlist simulation and negative controls
have run locally. **Vivado implementation and every XSim phase remain unverified.**
No Vivado executable or `settings64.sh` was found in accessible local installation
locations, filesystem searches, desktop/shell configuration or Podman images.
The Tcl/XSim flow is prepared but has not run against a real Vivado installation.

[ibex.json](ibex.json) pins the Apache-2.0 Ibex v2.1.0 source commit, prepared RTL
hash, configuration and Icarus/vvp versions. Preparation is shared with the
[open-hardware campaign](../benchmarks/open-hardware/README.md). No reference-tool
source or vendor implementation was inspected or translated. The verification
models in `primitives.v` were independently written from public AMD specifications.

Local evidence snapshot: [2026-10-23 initial campaign](results/2026-10-23-initial.json)
(`overall_status: unverified`; local checks and external tools are listed separately).

## Build and synthesize

Use the rustup installation meeting this repository's Rust 1.97 requirement and
a fresh target directory to preserve frozen benchmark executables:

```sh
mkdir -p .work/fpga
free -h
systemd-run --user --scope -p MemoryMax=8G -p MemorySwapMax=2G \
  /usr/bin/time -v -o .work/fpga/build.time timeout 600s \
  env CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=.work/fpga/target \
  cargo build --no-default-features -p rustsys-cli -p rusteda-cli

RS=.work/fpga/target/debug/rustsys
RE=.work/fpga/target/debug/rusteda
WS=.work/fpga/ibex
"$RE" -w "$WS" init
"$RE" -w "$WS" import --base-rev rev0 --top ibex_top --rtl /path/to/prepared/ibex.v
systemd-run --user --scope -p MemoryMax=2G -p MemorySwapMax=1G \
  /usr/bin/time -v -o .work/fpga/synth.time timeout 180s \
  "$RS" -w "$WS" --threads 2 --format json synth-fpga \
  --base-rev rev1 --part xc7z020clg400-1
```

The revision-checked transaction writes one hash-checked `fpga` directory table,
depending on `rtl`: `mapped.v`, `fpga.json`, and `luts.bin`. The JSON envelope
returns artifact paths. Parameterized primitives do not enter the ASIC netlist
schema. The binary side table is contiguous little-endian data suitable for
read-only mmap; each 40-byte record is root `u32`, leaf count `u32`, six leaf IDs
`u32`, INIT `u64`. The report identifies format version 1.

Covering is deterministic topological DP followed by one reverse marking scan.
Default configurable limits: 1 million nodes, 8 cuts/node (including trivial
cuts), 64 million cut merges, 512 MiB mapper storage, 256 MiB output. Cut limits
must be 2–32. No paths are enumerated. Budget exhaustion returns
`INPUT_REJECTED` with `FPGA_*_BUDGET` and does not advance HEAD.

Upfront memory: at the maximum, `1M × 8 × sizeof(Cut)` (48 bytes on this host),
plus vector headers, fanout/depth/flow/live tables, allocator margin and output
records, gives a checked mapper bound near 505 MB. Reserve 2 GiB including
frontend and text output. The frontend also needs an OS process cap. Use two build
jobs and two test workers. The campaign uses cgroups or OS address-space limits,
wall/file caps, a shared lock and `/usr/bin/time -v` peak RSS measurements.

## Reproducible campaign

The driver accepts an expected workspace revision, initializes a fresh workspace
when appropriate, imports pinned RTL, and synthesizes natively. Each run freezes
its verification inputs and stores hashes, tool versions, argv, wall time and
RSS under `WORKSPACE/verification/`; old receipts survive. Use one shared lock
across worktrees.

```sh
python3 verification/fpga/run.py \
  --workspace .work/fpga/ibex-acceptance --base-rev rev0 \
  --rtl /path/to/prepared/ibex.v \
  --rustsys .work/fpga/target/debug/rustsys \
  --rusteda .work/fpga/target/debug/rusteda \
  --iverilog /path/to/iverilog --vvp /path/to/vvp \
  --lock-file /path/to/shared/scale.lock
```

Missing Vivado produces **`unverified`, exit 2**, after available independent
simulation runs. Failed checks exit 1. With Vivado installed, source its
`settings64.sh` and add `--vivado /path/to/Vivado/VERSION/bin/vivado
--vivado-version VERSION`; the expected version must match actual output.

`implement.tcl` sets the source fileset to `DESIGN_MODE GateLvl`, directly reads
the structural RustSys netlist with `read_verilog` and `link_design`, then runs
`opt_design`, `place_design`, `route_design`.
It never calls `synth_design`. It checks unresolved black boxes, full routing,
routing errors, blocking DRC violations, setup slack at 100 ns and hold slack
with both Fast/Slow min/max analysis enabled. It exports
checkpoints, utilization/timing/DRC/route reports, functional/timing Verilog and
slow-corner SDF. Unknown unconstrained endpoint counts are not reported as zero.

XSim stages compare RTL with behavioral, post-synthesis functional/timing and
post-implementation functional/timing DUTs. Functional models bind to UNISIM;
timing models bind to SIMPRIM. Routed SDF may encode net delays with `PORT` or
`INTERCONNECT`; both are accepted alongside required cell IOPATH delays. Timing uses explicit
`xelab -sdfmax /ibex_tb/dut=...`, transport delays and enabled timing checks.
SDF errors are not suppressed. Actual SDF/timing warnings or failures reject the
stage before its receipt can pass; explicit zero-diagnostic summaries are allowed.
Dynamic timing coverage is Slow/max, while static timing enables Fast/Slow
min/max. Inputs launch 2 ns after the edge, including bus responses and grants,
and comparison occurs near the end of the cycle. These scripts require validation
in real Vivado.

## Scope and controls

The hand-encoded RV32IM program checks independent store signatures
10, 4, 14, 40, 13, 1, `0x600d`; load-use behavior, branch, multiplication,
division/remainder; two bus-stall sequences; and a repeated asynchronous reset.
Each seed compares 1500 cycles: requests/status, active bus payloads, write
integrity and defined crash-dump bits. Inactive bus payloads and unknown reference
diagnostic bits are not observations. This is **reset-qualified differential
simulation, not formal/arbitrary-state equivalence or full ISA coverage**.
Every source state is retained without reset assumptions, constant-state pruning
or merging. Low-active asynchronous controls use shared explicit LUT1 inversions,
avoiding optional reset-inversion attributes whose physical support differs
between 7 Series and UltraScale. Programmable clock polarity still uses the
documented IS_C_INVERTED parameter; acceptance by the selected real tool version
remains to be verified. Physical FPGA INIT/GSR behavior comes from vendor defaults.

Independent primitive models start state unknown and omit physical GSR/timing.
LUTs use Shannon mux expansion: `INIT[unknown_address]` alone introduces avoidable
X pessimism when every possible selected value agrees. Vendor UNISIM/SIMPRIM
libraries are required for XSim stages.

The probe covers LUT INIT/input ordering, both edges, asynchronous set/clear,
enabled registers, latches and generated-name collisions. Corrupted LUT, reset
polarity and clock polarity must produce actual mismatches; compile errors and
timeouts cannot pass negative controls:

```sh
python3 verification/fpga/run.py \
  --workspace .work/fpga/probe --base-rev rev0 --design probe \
  --rustsys .work/fpga/target/debug/rustsys \
  --rusteda .work/fpga/target/debug/rusteda \
  --iverilog /path/to/iverilog --vvp /path/to/vvp
```

## Tool access and continuation

A real Vivado installation with XSim and Zynq-7000 device data is required. Lab
Edition alone supplies programming/debug tools and cannot satisfy this flow.
The [official 2025.2 Linux web installer](https://www.amd.com/en/support/downloads/adaptive-socs-and-fpgas/development-tools/2025-2.html)
was checked and redirects to AMD account sign-in. The login page is open for the
user; no passwords or authentication tokens are collected by this repository.
Official installation also requires the user's license-agreement acceptance.
An already-authorized installation/executor can be supplied instead.

Once tools are available, run the same campaign with their path and exact
version; all prior receipts remain intact. An existing campaign workspace can
use its current `revN` as `--base-rev` to import a new source revision and run
fresh verification without replacing history.

Harness regression checks (small Python worker/child, estimated <128 MiB):

```sh
systemd-run --user --scope -p MemoryMax=1G -p MemorySwapMax=0 \
  /usr/bin/time -v timeout 30s python3 verification/fpga/test_run.py
```

Public specifications: [AMD UG953 LUT6](https://docs.amd.com/r/en-US/ug953-vivado-7series-libraries/LUT6),
[FDCE](https://docs.amd.com/r/en-US/ug953-vivado-7series-libraries/FDCE),
[FDPE](https://docs.amd.com/r/en-US/ug953-vivado-7series-libraries/FDPE),
[UG904 third-party netlists](https://docs.amd.com/api/khub/documents/eeESP3eiV0LaWhrlmBYX8Q/content),
[UG835 write_verilog](https://docs.amd.com/r/2023.2-English/ug835-vivado-tcl-commands/write_verilog),
[route status](https://docs.amd.com/r/en-US/ug835-vivado-tcl-commands/report_route_status),
[UG900 XSim options](https://docs.amd.com/r/en-US/ug900-vivado-logic-simulation/xelab-xvhdl-and-xvlog-xsim-Command-Options).
