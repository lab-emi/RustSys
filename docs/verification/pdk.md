# Dual-PDK suite

`cargo xtask verify pdk` synthesizes every fixture design with the standard-cell libraries of two
process design kits and checks each netlist with the synthesis checker. The libraries are pinned
in `verification/pdks.toml` and downloaded by `cargo xtask pdk fetch`. They are never tracked:
design data is tracked only as synthetic fixtures, and each library is 12 to 20 MB.

## Coverage

| Name | Library | Corner | Source form |
|---|---|---|---|
| `sky130hd_tt` | `sky130_fd_sc_hd` | `tt_025C_1v80` | one Liberty file |
| `sky130hd_ss` | `sky130_fd_sc_hd` | `ss_100C_1v60` | one Liberty file |
| `sky130hd_ff` | `sky130_fd_sc_hd` | `ff_n40C_1v95` | one Liberty file |
| `gf180mcu7t_tt` | `gf180mcu_fd_sc_mcu7t5v0` | `tt_025C_3v30` | header and 229 cell fragments |
| `gf180mcu9t_tt` | `gf180mcu_fd_sc_mcu9t5v0` | `tt_025C_3v30` | header and 229 cell fragments |

Every design of `verification/fixtures/synthetic/synth/rtl/index.toml` runs with every library
(31 designs and 155 runs today), with the default synthesis options and no `--dont-use` patterns.
A full run takes about a minute on a workstation.

## Additional IHP SG13G2 benchmark library

The separate five-design synthesis benchmark also uses
[ETH Zurich's EZ130 8T v1.1](https://iip.ethz.ch/ez-library/ihp130-8t.html)
on IHP SG13G2 130 nm, at **TT, 1.20 V, 25°C**. This supplements the required
Sky130/GF180 fixture matrix above.

| Field | Pinned value |
|---|---|
| Library | `ez130_8t` / EZ130 8T v1.1 |
| Benchmark ID | `ez130_8t_tt` |
| Liberty file | `ez130_8t_tt_1p20v_25c.lib` |
| License | Apache-2.0 |
| Liberty SHA-256 | `847cb30b38f15ef0b9c4a735cfaf96c1782c013b1952261b25e5e38cd3ae072c` |

`verification/benchmarks/open-hardware/additional-pdks.json` pins the archive,
member, size, checksum and latch wrappers. `fetch_additional_pdks.py` verifies
both compressed and decoded bytes before writing the Liberty file. An existing
file with different bytes is preserved and rejected. The required baseline PDK
manifest and fixture coverage stay separate.

The release's TIEHI/TIELO Liberty driver-type tags conflict with its own
constant-output Verilog models. Benchmark functional LEC uses those pinned
vendor Verilog constants through an explicitly recorded supplement, with
external positive and corrupted-constant checks. Original synthesis and timing
library bytes stay pinned. This scope covers functional logic under nominal
cell models; electrical strengths and high impedance remain outside the proof.

See the [EZ130 CLI example](../guide/synthesis.md#use-ez130-cells-on-ihp-sg13g2)
and [benchmark protocol and evidence](../guide/benchmarks.md). Please cite
Castañeda et al., [An Open-Source Standard-Cell Library for IHP 130 nm Developed by Students](https://arxiv.org/abs/2609.29965).

## Sources and licenses

Every file comes from the vendor repository at a pinned commit, through the raw-file address of
that commit. `verification/pdks.toml` records the commit, the license, the SHA-256 and the size of
each library file.

| Repository | Commit | License |
|---|---|---|
| <https://github.com/fossi-foundation/skywater-pdk-libs-sky130_fd_sc_hd> | `9cb2d7cb8ed4619094263614039a61b6b2d22a88` | Apache-2.0 |
| <https://github.com/fossi-foundation/globalfoundries-pdk-libs-gf180mcu_fd_sc_mcu7t5v0> | `0ef8889a8df0acf959683fbe73013f342a2206a2` | Apache-2.0 |
| <https://github.com/fossi-foundation/globalfoundries-pdk-libs-gf180mcu_fd_sc_mcu9t5v0> | `02b7af3f8d88c0b014d9af3db6a17f02aa983305` | Apache-2.0 |

The sky130 libraries are the files `timing/<library>__<corner>.lib`. The gf180mcu repositories
hold no combined Liberty file: `liberty/<library>__<corner>.lib` is a header with the library
attributes, and each cell is a fragment `cells/<cell>/<library>__<cell>[_<drive>]__<corner>.lib`.
The fetch assembles them into `<library>__<corner>.lib`:

1. the header without its last line `}` and the blank lines after it;
2. for each fragment, in the order the manifest lists them, a newline and the fragment;
3. a newline, `}` and a newline.

The manifest lists the fragment paths in version order of their file names (`sort -V`: digit runs
compare as numbers, letters sort before other characters). With the list, the assembly can be
repeated without network from a checkout of the vendor repository at the commit, and the SHA-256
of the result verifies every part and their order.

## Fetch

```sh
cargo xtask pdk fetch                    # into target/pdk
cargo xtask pdk fetch --dir ../pdk-libs  # or RUSTSYS_PDK_DIR=../pdk-libs
```

- The library directory is `--dir`, else `RUSTSYS_PDK_DIR`, else `target/pdk` of the repository.
  Inside the repository only directories that git ignores and that hold no tracked file are
  accepted.
- `curl` downloads, `sha256sum` (or `shasum -a 256`) hashes; a missing program is an error that
  names it.
- A library file that is present with the recorded size and SHA-256 is kept without network
  access. A library file is written to a `.part` file and renamed into place only when its size
  and SHA-256 match the manifest; on a mismatch the command fails and the file is not kept.
- Headers and fragments have no hash of their own. Each is renamed into `sources/<name>/` in the
  library directory when its download completes, and is checked through the SHA-256 of the
  assembled file. The parts stay there, so a damaged or deleted library file is assembled again
  without network. When the kept parts do not assemble (for example a page that a proxy served in
  place of a file), or their assembly does not match its SHA-256, they are downloaded once more
  before the command fails.
- A failed download is attempted up to three times, except when another attempt cannot change
  the result: a refused connection, an HTTP status of 400 or more, or a timeout (curl itself
  retries timeouts and the statuses 408, 429 and 5xx). No download of parts starts after one has
  failed; the parts downloaded until then are kept for the next fetch.
- Every library is attempted; the failures are listed together at the end.

## Run

```sh
cargo xtask verify pdk                   # skips with a banner when libraries are missing
cargo xtask verify pdk --require         # missing libraries fail the suite
cargo xtask verify pdk --dir ../pdk-libs --timeout 300
```

1. Libraries missing from the library directory print a SKIPPED banner. Without `--require` the
   command then runs the libraries that are present and exits 0 when none is; with `--require`
   it exits 1 before building anything. Present libraries must match their size and SHA-256.
2. The command line is built once:
   `cargo build --release -p rustsys-cli --no-default-features`, into cargo's target directory.
3. For each library and design:

   ```sh
   rustsys --progress off synth --top TOP --lib LIB --out netlist.v --report report.json SOURCES
   rustsys --progress off check synth --top TOP --lib LIB --netlist netlist.v --report check.json SOURCES
   ```

   Each invocation has a time limit (`--timeout`, 600 s by default) and is killed when it passes.

Outputs go to `target/verify-pdk/`: one directory per library and design with the netlist, both
reports, the standard output and error of both steps and `command.txt` (the two command lines),
plus `summary.txt` (the printed summary) and `runs.tsv` (one line per run). The directory is
replaced by each run.

## Assertions

A run passes when synthesis exits 0 and the check exits 0 with the verdict `equivalent`: every
netlist is proven to implement its RTL by the independent checker (SYNTH-REQ-0801). The command
exits 1 when any run fails, after listing each failure with the first error line of the failing
step. A fixture index without designs is an error before anything is built, so the command never
passes on zero runs.

## Reports

These are printed and written, never asserted:

- Physical-usability hazards, per run and as totals per library:
  - instances of cells whose Liberty cell group, or a group inside it such as a pin, sets
    `is_isolation_cell`, `always_on` or `is_level_shifter` to true;
  - instances of cells whose name contains `sparecell`;
  - literal constants `1'b0` and `1'b1` in the netlist: `assign` statements, and cell pins
    connected to a constant.
- QoR: cells and area of every design with every library, with totals over the passed runs. Area
  is in the library's area unit.

## Known hazards

Both kits pass every run. The netlists are equivalent to their RTL, but some of them use cells or
constants that a physical implementation flow may not accept as they are. Seen with the
libraries above:

- Cell selection reads functions, pins and `dont_use` (SYNTH-REQ-0501), not the attributes
  `is_isolation_cell`, `always_on` and `is_level_shifter`. `sky130_fd_sc_hd` marks 11 isolation
  cells, 12 cells with `always_on` (on the cell or on a pin) and 7 level shifters.
  - Its isolation buffers (`lpflow_isobufsrc_*`, `lpflow_inputiso*`) implement ordinary logic
    functions and appear in netlists at every corner.
  - The isolation latch `lpflow_inputisolatch_1` implements the latches of the `latches` design.
  - At the ff corner, `rv_core` uses `lpflow_clkbufkapwr_1`, an always-on buffer on the `KAPWR`
    supply.
  - The gf180mcu libraries mark no cell.
- The tie cell `conb_1` of `sky130_fd_sc_hd` has two outputs, so it is not used
  (SYNTH-REQ-0501). Constant 0 is driven by `macro_sparecell`, a single-output cell whose output
  function is 0. Constant 1 has no tie cell and appears as `assign … = 1'b1` (SYNTH-REQ-0522).
  Excluding the spare cell with `--dont-use` makes constant 0 a literal as well. The gf180mcu
  libraries have single-output tie cells (`tiel`, `tieh`) and show neither.
- Neither kit marks any cell `dont_use`. Not reported by the suite: clock inverters implement
  ordinary inversions. `clkinv_1` has the area of `inv_1` and comes first in the file, so almost
  every inverter in the netlists is a `clkinv` cell.

`--dont-use` with name patterns (for example `--dont-use '*lpflow*'`) excludes cells from
selection.

## Changing the manifest

Each `[[library]]` in `verification/pdks.toml` has `name`, `library`, `corner`, `repo`,
`commit`, `license`, `sha256` and `size`, and either `url` (a whole file) or `base_url`,
`header` and `fragments` (an assembled file). Every URL must contain the commit. After a change,
`cargo xtask pdk fetch` downloads the new files and fails until `sha256` and `size` describe the
library file it produced. `cargo test -p xtask` checks the set of libraries, that every URL points
into its repository at its commit, and the layout and order of the fragment lists.
