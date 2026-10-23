# Open hardware synthesis benchmark

This benchmark compares the complete Ibex, PicoRV32, SERV, VexRiscv and OpenC910 designs with
RustSys and the Yosys executable. All upstream RTL is fetched at pinned revisions;
reference EDA tools are used only as black-box executables.

## Common multicore protocol

`scaling.py` applies the same one-warm-up/three-measured-run protocol to all five
designs and both PDKs. Both tools are measured on one and sixteen physical host cores.
Report actual CPU seconds divided by wall seconds,
not requested workers. Every selected repetition must finish. Netlist hashes and
QoR must agree across repetitions. The unified table requires byte-identical one-/sixteen-core
netlists from each tool before sharing its area, timing and proof across allocations.
Process startup, workspace init/import, synthesis, mmap and full
Verilog export are inside the timer. Source preparation, proof and STA are outside.

The common reference uses the initial-state-preserving public Yosys flow in
`yosys_initial.py`, with structural memory lowering, `-keepdc` transformations,
explicit ordinary X/Z-data completion, and **default ABC library mapping** for
every design. It adds no reset or initialization assumption. `--reference-full-map`
selects this profile, including OpenC910; neither `-fast` nor the older four-cut
mapping script is used. Case promises are normalized before parsing. Older
standard-flow and bounded-mapping samples retain their own profiles and are
not substituted into these medians.

In the v3 flow, ordinary X/Z data is completed **before** flip-flop optimization.
This preserves the arbitrary current value of X-next-state registers until their
first clock. Later `opt_dff -keepdc` enable extraction remains enabled. The
`scaling_profile_controls.py` fixture requires an external counterexample for
the earlier order and a complete proof for v3 on both PDKs, covering FFs,
independently initialized duplicate-D registers, a latch and memory bits.

Before running, prepare the pinned small-core inputs and full OpenC910 source as
described below, fetch the pinned PDKs, and build release producers. Freeze the
executables before starting a campaign. Current planning for the full core is
under 20 GiB RustSys RSS (shared 4 GiB cuts plus graph/frontend/netlist buffers
and worker scratch); the opaque reference has an explicit 32 GiB experimental
ceiling. Each child has a 32 GiB RAM / 1 GiB swap cgroup and independent wall/file
limits; a larger reference allocation is not presumed to complete. Inspect host
RAM first and serialize high-memory runs behind `.work/scale.lock`. The runner
measures actual peak RSS and waits within a bounded budget for available RAM.

An example using local preparation manifests and explicitly frozen tools:

```sh
free -h
mkdir -p .work/bench-binaries
cp target/release/rustsys target/release/rusteda .work/bench-binaries/
(
  ulimit -v 41943040
  /usr/bin/time -v -o .work/multicore-driver.time timeout 86400s \
    python3 verification/benchmarks/open-hardware/scaling.py \
    --workspace .work/multicore-current --binaries .work/bench-binaries \
    --inputs .work/open-hardware/inputs.json \
    --large-input .work/c910-prepared/input.json --pdks target/pdk \
    --yosys /path/to/pinned/yosys --cores 16 1 \
    --reference-full-map --repetitions 3 --warmups 1 \
    --memory-gib 32 --timeout 10800
)
```

Use a fresh output directory, or `--resume` only for the same input/tool/limit
configuration. Optional `--reference-executables` pins the actual binaries behind
launchers. The exact pinned Yosys version is checked before measuring. On a host
with fewer physical cores, select supported core counts and label a different
profile rather than calling SMT siblings extra physical cores.

Quality experiments can explicitly select `--goal area|delay`, `--effort`,
`--timing-period` (ns), `--area-limit` and `--buffer-fanout`. These profiles use
the same complete four-command workspace pipeline and remain inside the timer.
`--profile-commands` additionally records complete init/import/synth/export CPU
and wall times. Every profile is recorded in the configuration and resume checks.

For a constrained Yosys comparison, `--reference-timing --reference-period 13`
uses public ABC timing constraints and its default buffer/upsize/dnsize flow,
with a pinned driving cell and `--reference-load-ff 10`. Targets are passed in
plain decimal ps as required by ABC. This retains the v3 arbitrary-state flow;
it does not introduce `-dff`, reset assumptions, a custom reduced-effort mapping
script or a short-cut timer. Compare actual area and independent OpenSTA timing;
neither tool is presumed to meet a requested target.

`profile_verify.py` can prove and time selected 16-core scout results before a
full campaign exists. It retains their actual repetition count and never turns
a single sample into a published three-run median. `parallel_opt_cec.py` checks
complete before/after AIG pairs for the optimization passes; `enabled_seq_verify.py`
checks real-PDK enabled/feedback cells and rejects deliberate enable/clock faults.

`scaling_verify.py` checks the actual exported Verilog with the independent RTL
event model and external ABC. All original outputs and retained state data,
clock/enable and asynchronous controls are kept; original inputs and RTL state
are quantified. The common language model completes ordinary X/Z data and
undriven wires to zero **without initializing registers**. Every implementation
initial state remains covered. Export itself stays undecided until external CEC
completes. `sta_benchmark.py` separately runs opaque OpenSTA under the same TT,
ideal-clock/zero-wire setup model for both tools. Its reported setup-Fmax is not
routed or signoff Fmax.

For inferred multiplication/addition, `observed_proof.py` captures an untrusted
pre-ABC checkpoint using the same public v3 commands. Optional native observations
add operand/result obligations without changing the original interface or state
quantification. Arisca checks exact extracted arithmetic components. ABC checks
each extraction and context reconstruction, the complete Boolean context, the
original source frame, and the actual measured mapping. Only their complete
composition is a proof. Model exports retain undecided status. The checker adds
no reset sequence, state choice or input restriction.

Build the separate development observation driver before freezing verification
tools, then pass explicit tool paths when they differ from the documented local
layout:

```sh
free -h
(
  ulimit -v 12582912
  /usr/bin/time -v -o .work/check-tools-build.time timeout 1800s \
    env CARGO_BUILD_JOBS=1 cargo build --locked --release -p rustsys-check \
    --example check_config --example check_probe
)
(
  ulimit -v 41943040
  /usr/bin/time -v -o .work/multicore-validation.time timeout 86400s \
    python3 verification/benchmarks/open-hardware/scaling_verify.py \
    --workspace .work/multicore-current --directory .work/multicore-validation \
    --sta /path/to/OpenSTA/sta --phase all \
    --observation-checker target/release/examples/check_probe \
    --arisca /path/to/pinned/arisca
)
```

Run the complete verification worker under the memory/time guards described
above. Per-child native/solver scopes are also enforced by the harness. Keep
the actual pinned OSS CAD Suite executables used by the proof receipts; do not
mix incompatible Yosys/EQY plugin builds. The current arithmetic composition
uses standalone AIGER interfaces and no reference-tool implementation source.

`sta_structural.py` handles OpenSTA's rejection of unused signed loop-wire
declarations. It removes only their signed marker after rejecting every
functional use; ports, cells, bits and connectivity stay unchanged. Original
and adapted bytes and the edit ledger are hashed. Failed reader attempts are
retained separately from successful timing receipts.

`scaling_report.py` publishes only a complete selected matrix with three timed
samples, complete proofs, timing receipts, and dual-PDK data/clock corruption
controls. It verifies default mapping scripts and exact artifact hashes, retains
large artifacts by hash, and chunks evidence above the repository's per-file
size limit. `scaling_join.py` can combine immutable RustSys controls with a
separately collected common reference; it never blends samples across profiles. It checks
only the binaries actually used by each selected tool pipeline and retains both source
campaigns, so an unused RustSys executable from a previous Yosys campaign is not
misrepresented as a current producer.

For the unified one-/sixteen-core matrix, `unified_join.py` accepts one complete RustSys
campaign and one or more `--reference` campaigns covering the actual Yosys allocations.
It requires all 40 design/library/tool/core configurations, each with one warm-up and three
measured samples. It checks the used executables, inputs, raw samples and both tools' netlist
identity across allocations. Missing one-core runs cannot be inferred from observed CPU use.
The publisher displays both tools and allocations in one table; reused reference samples
retain their original source-campaign provenance. Different netlists need separate timing
and proof evidence before their quality can be shared.

`scaling_verify.py --reuse-proofs FILE --reuse-timing FILE` accepts explicit
previous complete records. Proof reuse requires identical RTL, library and netlist
bytes; timing reuse additionally requires the same OpenSTA executable and analysis
constraints. Input/artifact hashes are checked and reuse is labeled in the record.
A changed or incomplete result requires a fresh verification run.

The default archive includes solver receipts and reproducible model payloads
up to 32 KiB each, with a 256 MiB copied-data budget and a 65,536-file scan cap.
Larger model files remain in the local workspace and the SHA-256 inventory;
the publication records its exact limits. Archive parts are at most 1 MiB.

## IHP SG13G2 / EZ130 8T extension

`additional-pdks.json` pins ETH Zurich's [EZ130 8T v1.1](https://iip.ethz.ch/ez-library/ihp130-8t.html)
at TT, 1.20 V, 25°C, including the original download and decoded Liberty SHA-256.
The library is Apache-2.0; cite [Castañeda et al.](https://arxiv.org/abs/2609.29965).
This optional benchmark library supplements the required Sky130/GF180 baseline;
it does not replace the shared `verification/pdks.toml` or its dual-PDK tests.

Fetch only the pinned Liberty data under a 2 GiB address-space / 120 s guard:

```sh
(
  ulimit -v 2097152
  timeout 120s python3 verification/benchmarks/open-hardware/fetch_additional_pdks.py
)
```

Use the exact frozen producers and prepared inputs from the common campaign.
The extension measures all five designs at **16 physical cores**, with the
same one-warm-up/three-repetition protocol, full CLI timing and default mapping.
It does not infer one-core results or within-tool CPU scaling for the new library.
Inspect available RAM and keep large cases serialized:

```sh
free -h
(
  ulimit -v 41943040
  /usr/bin/time -v -o .work/ez130-driver.time timeout 86400s \
    python3 verification/benchmarks/open-hardware/scaling.py \
    --workspace .work/ez130-measurements --binaries .work/bench-binaries \
    --extra-pdks verification/benchmarks/open-hardware/additional-pdks.json \
    --library ez130_8t_tt --cores 16 --reference-full-map \
    --repetitions 3 --warmups 1 --memory-gib 32 --timeout 10800
)
```

`latches-ez130.v` binds the Liberty-defined positive and negative latch functions;
reset wrappers preserve arbitrary initial state and give the reset input priority.
The reference uses these wrappers with ordinary default ABC mapping, adding no
reset sequence or initialization assumption. Every exported implementation still
needs its own complete RTL proof.

After measurement finishes, run `scaling_controls.py --library target/pdk/ez130_8t_tt_1p20v_25c.lib`
and `scaling_profile_controls.py` with the same `--library` and
`--latch-map verification/fixtures/synthetic/lec/latches-ez130.v`, each in a fresh
`--directory`. Their existing per-child memory and time guards apply. Require a
positive proof, explicit external data/clock counterexamples, and the v1/v3
arbitrary-state calibration. Retain the outputs as `controls.json` and
`profile-controls.json` in a common controls directory.

OpenC910's mapped reference loses some public state names. Before LEC, capture
an **untimed** public pre-ABC Verilog checkpoint and derive bounded, untrusted
state hints. This invokes the same measured public script prefix; it does not
change the timed netlist. Run under a 40 GiB address-space / 2600 s parent guard:

```sh
python3 verification/benchmarks/open-hardware/ez130_state_hints.py \
  --config .work/ez130-measurements/openc910/ez130_8t_tt/16c/yosys-3/config.json \
  --directory .work/ez130-state-hints
```

The capture has its own 32 GiB physical-memory limit and scale lock. Existing
public checkpoints can be supplied with `--preabc FILE` under a 2 GiB / 180 s
guard instead. Matching uses scalar-Q order, public name anchors and explicit
inverter aliases; it never accepts the result as equivalent.

Freeze an updated `rustsys-check` development `check_config` executable in a
separate build target, preserving all measured producers. Run
`additional_verify.py --workspace .work/ez130-measurements --directory .work/ez130-validation --sta /path/to/pinned/sta --event-checker /path/to/frozen/check_config --state-proposals-file .work/ez130-state-hints/proposals.json --cec-solver new`
under the same guarded parent invocation. It performs fresh independent event
LEC, ABC/Arisca arithmetic composition where required, and OpenSTA for all ten
new netlists. Changed-library certificates cannot be taken from the baseline.
Full-core exports use a 24 GiB guard; full-core STA uses 32 GiB, one case at a time.
Actual child peak RSS, stdout, stderr and all artifact hashes remain in the receipts.

For the separately reported critical-path load scope, run
`inspect_timing_load.py --validation .work/ez130-validation --library ez130_8t_tt --record-any-load`
under a 2 GiB / 60 s guard. It inspects the largest reported cell delay on the
worst same-clock/same-edge path, with no new path enumeration. The selected
load and its Liberty limits are observations, not a global electrical pass.

`additional_report.py --workspace .work/ez130-measurements --validation .work/ez130-validation --controls .work/ez130-controls --output verification/benchmarks/open-hardware/results/YYYY-MM-DD-ez130 --update-docs`
publishes only the complete, bound five-design matrix. Use a 2 GiB / 600 s parent
guard. The combined overview retains all baseline rows and adds EZ130 area,
setup-Fmax, process RSS and speedup rows in README, documentation homepage and
benchmark guide. Its archive retains the sampling, proof and timing scope.

The EZ130 release's TIEHI/TIELO Liberty `driver_type` attributes conflict with
its own Verilog models (`Y=1` and `Y=0`). `functional_models.py` checks the pinned
vendor models with external ABC, including corrupted-constant controls, and
records an explicit two-attribute functional supplement. Functional LEC uses
these nominal vendor constants. The original synthesis and timing library
bytes remain pinned; electrical strengths and high impedance are outside the
functional proof. Other unsupported driver types remain errors.

When mapping removes state wire names, the event checker can propose bounded
one-to-one RTL/state bindings. Proposals introduce no initial-state choices,
constants, sharing or reset assumptions. Every original RTL state stays free,
including states excluded from the proposal search. Complete external checks
of all outputs and retained data/clock/asynchronous controls are still required.
Definition, candidate, support-storage, traversal, round and evaluation limits
stop discovery as undecided when exhausted. Use a separate build target and
pass a frozen `additional_verify.py --event-checker PATH --attempt HANDLE` to
preserve earlier diagnostics and the measured producers. `validate_state_proposals.py`
provides six external positive/data/clock/encoded-state controls for this exporter.
Use `--cec-solver new` with the optional public ABC `-x` algorithm; an incomplete
result can retry the bounded default algorithm on the identical AIGER interface.
An explicit counterexample is final. Export and partial proofs remain undecided.
Run it with the same frozen `--checker` and
`--directory .work/ez130-controls/state-proposals` under a 2 GiB / 600 s parent
guard. Publication requires those controls too; `additional_report.py
--state-controls FILE` accepts a separately named control receipt.

## Large-design stress run

This section describes the historical single-sample stress profile. The common
multicore protocol above supersedes its sampling and reference mapping settings.

`large-designs.json` additionally pins the Apache-2.0 OpenC910 project at
`b91c90914c19f114d35c8f6b73408eb241ed847c`. Its `ct_core` top is one complete
core with the upstream default 64 KiB instruction and 64 KiB data caches.
Functional FPGA RAM models are mapped to standard cells, with no SRAM black boxes.
L2, a second core and cluster peripherals are outside this top. This is a separate
stress profile; the four-core configuration and historical measurements below
remain independently pinned.

The prepared source is 20,115,079 bytes with SHA-256
`0ec2601635ca3196cf57fce4b8d5f58227b2b7b9e06fce81605e4c505cee9e56`.
Only Verilog preprocessing is shared: Icarus expands the upstream includes/macros;
it performs no synthesis or elaboration. Each timed tool then performs its own
elaboration, memory lowering, synthesis, cell mapping and full Verilog export.

Upfront plan: the inventory has 235,355 word cells, 353 memories and 1,454,592
memory bits. After lowering, RustSys has 13.1 million AIG AND nodes. Unbounded
per-root path storage and fixed 40-cut allocations would exceed the intended RAM
guard. The engine now bounds mux-tree candidates, resubstitution side-edge visits
and total cut storage. The default 4 GiB cut budget clips this core's medium-effort
priority-cut limit from 40 to 7; the report records both numbers. This changes the
mapping search/area tradeoff. No RTL block or cache is removed to meet a budget.

The large harness uses a child-only 16 GiB physical-memory / 1 GiB swap cgroup,
one worker pinned to the same logical CPU, a 3,600-second deadline per tool,
2 GiB per output file and a 48 GiB / 200,000-file workspace cap. Its small
reporting parent survives a child memory failure. Runs above 4 GiB serialize
behind `.work/scale.lock`. Streaming export avoids thawing the whole gate graph
or building a multi-gigabyte Verilog string. Full LEC has a configurable guard
(8 GiB by default; 24 GiB for the large follow-up);
it is never inferred from synthesis, matching counts, or small regression proofs.

The initial Yosys 0.67 default-profile probe exhausted the 16 GiB memory limit.
The additional `--yosys-noshare` profile uses Yosys's public `synth -noshare`
option and is labelled separately in every result. This is not an assertion that
Yosys cannot complete with different resources or settings. A single measured
run per tool and PDK is used for the stress table; it is not a three-run median
or a repeat-determinism claim.

From the repository root, after building release binaries and fetching the
pinned PDKs in `verification/pdks.toml`:

```sh
git clone https://github.com/XUANTIE-RV/openc910 .work/openc910
git -C .work/openc910 checkout --detach b91c90914c19f114d35c8f6b73408eb241ed847c
free -h
python3 verification/benchmarks/open-hardware/large.py prepare \
  --source-root .work/openc910 --workspace .work/c910-prepared \
  --preprocessor /path/to/iverilog
systemd-run --user --scope -p MemoryMax=2G -p MemorySwapMax=512M \
  timeout 16000s python3 verification/benchmarks/open-hardware/large.py run \
  --workspace .work/c910-measured --prepared .work/c910-prepared \
  --yosys /path/to/pinned/yosys --memory-gib 16 --timeout 3600 \
  --cpu 0 --warmups 0 --repetitions 1 --yosys-noshare
```

Do not rebuild the producer binaries during a measurement. The harness records
their initial hashes and verifies them again before accepting the completed
dataset. Save a producer source snapshot with exact build flags. Failed runs
retain their commands, status and logs; unavailable QoR/RSS is not reported as
zero. Successful runs must contain no unmapped logic cells. Publication through
`large_report.py` rechecks the complete prepared-core and netlist hashes and
requires both PDK rows. Only a complete external certificate can display
`Proven`; omitted large artifacts retain their hashes and sizes.

Large LEC can use `large_lec.py` with the independent checker example and an
opaque ABC executable. The native RTL interpreter and direct mmap gate reader
export every original output bit and every retained state clock/enable, clear,
preset and next-data obligation. All RTL input/state variables are quantified;
a serialized one-to-one named relation must cover every implementation state.
Removed or aliased reference registers remain independent free variables; they
are never assumed constant or equal. ABC must check that they do not affect any
original output or retained state transition. No reset pulse or initial-state
choice is accepted. ABC compares both complete
combinational AIGER interfaces. Export itself remains `undecided`; only a
complete external verdict can publish `Proven†`.

Arbitrary initial state is stricter than equivalence after reset. The measured
default `synth -noshare` OpenC910 netlists merge the two observable `lpmd_b` bits:
the reference permits initial `01` and `10`, while that implementation can only
represent `00` and `11`. ABC refutes an original top-level output and
`lec_counterexample.py` replays the witness in both original AIGER models.
Increasing RAM or a deadline cannot turn this counterexample into a proof.
`yosys_initial.py` is a separate, explicitly labelled reference profile using
structural memory lowering and public `opt_merge -keepdc` / `opt_dff -keepdc`
commands, omitting `wreduce` state elimination. It never uses `setundef -init`.
Its measurements and proofs must be attached to its own netlist hashes; they
cannot certify the earlier default-profile netlist.
For a quicker LEC reference baseline, its config can set `yosys_abc_fast: true`
to select the public `abc -fast` mapping command. This retains the same initial
state requirement. Runtime, area and proofs are specific to that separately
labelled profile; a faster mapping setting trades mapping effort for QoR.
The completed large follow-up uses `yosys_bounded_map: true`, the public ABC
script `strash; &get -n; &nf -C 4 -F 1; &put`, and a 32 GiB synthesis guard.
Four priority cuts per node and one area-flow round bound the mapping work.

Full-core reference calibration exposed a shared gap in synthesis and the
native checker: concatenated/replicated wildcard labels lost their masks.
Earlier OpenC910 native certificates are listed in the
[invalidation registry](results/proof-invalidations.json). The correction is
checked against a captured Icarus truth table for every one of 256 inputs,
Yosys formal comparisons on both PDKs, and negative old-netlist controls.
The new large proofs reimport each actual exported Verilog netlist. Calibrated
Yosys baseline proofs and corrected RustSys proofs retain separate source,
netlist, library and executable hashes.

This profile explicitly uses the native model's X/Z-literal-as-zero semantics.
The four-core campaign retains its separately documented undef-aware proof scope.
The exporter is checked on both PDKs against Yosys-produced netlists, data/clock/
reset corruptions, and a separate explicit eight-row truth table lowered to
AIGER directly by Yosys. Large proof models, state relations, counts and hashes
are saved for audit. Proof children have an independent configurable memory guard and
run serially after measurements behind the scale lock.

```sh
cargo build --release --locked -p rustsys-check --example check_config
systemd-run --user --scope -p MemoryMax=2G -p MemorySwapMax=512M \
  timeout 60000s python3 verification/benchmarks/open-hardware/large_lec.py \
  --measurements .work/c910-measured --workspace .work/c910-lec \
  --checker target/release/examples/check_config --abc /path/to/yosys-abc \
  --model-seconds 7200 --seconds 10800 --memory-gib 24 --swap-gib 1 \
  --export-only --reimport-netlists --cpu 0
```

When whole-network ABC exhausts its memory guard, `aig_slices` projects exact
output cones into batches. The two sides retain one common map to their original
input positions; unused inputs can be omitted because they do not occur in the
selected functions. Each batch contains the original AND/NOT equations, with
only exact constant and shared-structure identities folded. Every original
output and event/state obligation must occur in one interval of the coverage
ledger. No reset sequence, reachable-state restriction or new input assumption
is introduced. A `prepared` projection is not a proof. ABC must prove every
remaining projected output before the dataset can display `Proven†`.

```sh
cargo build --release --locked -p rustsys-check --example aig_slices
systemd-run --user --scope -p MemoryMax=2G -p MemorySwapMax=512M \
  timeout 30000s python3 verification/benchmarks/open-hardware/slice_lec.py \
  --original .work/c910-lec --workspace .work/c910-sliced-lec \
  --slicer target/release/examples/aig_slices --abc /path/to/yosys-abc \
  --cpu 8 --seconds 1800 --jobs 4
```

`--export-only` skips a whole-network solver attempt and remains unverified until
all exact projected obligations pass. The last deadline applies separately to
small projected batches. Failed batches
remain unverified and can be retried with larger budgets; no incomplete coverage
is accepted. Projection bounds the raw graph counts, local nodes, global edge
expansions, batch count and output bytes. Projection and each solver child use
4 GiB physical-memory guards and run one worker at a time. The OpenC910 Sky130
projection contains 5,954,905 obligations in 1,454 batches and measured under 296 MiB
peak process RSS. Independent controls include correct and corrupted data,
clock and reset functions on both PDKs, plus two independently generated AIGER
encodings with 1.5 million input positions and a corrupted final output. The
largest observed ABC slice used under 74 MiB process RSS on either PDK; all
batches completed, without reset or initial-state choices.

## Resource plan (before running)

- Host preflight: 59 GiB physical RAM, approximately 17 GiB available.
- Build: two Cargo workers, conservatively 2 GiB per compiler plus 2 GiB for the
  linker and shared inputs: approximately 6 GiB peak. Cap at 8 GiB RAM, 2 GiB swap.
- Synthesis: one design, one PDK corner, one worker at a time. Planning ceiling:
  1,000,000 graph objects x 256 bytes of node/arc/state data = 244 MiB. The initial
  cut estimate was refined to the medium-effort limit of 40 cuts per node, with
  a conservative 64 bytes per cut: 2,441 MiB. Add a 512 MiB front-end/library
  budget and a 1.5 scratch/allocator multiplier: approximately 4.7 GiB. Hence
  the exclusive scale lock and 8 GiB child limit. The full VexRiscv
  cache configuration was probed at 617,750 initial AIG nodes and 967 MiB RSS.
- Checking: one worker, 100,000 conflicts per RustSys SAT/model obligation;
  external proof steps have an independent deadline. No enumeration of all paths or cones. Cap at 8 GiB,
  with a separate wall-clock deadline; exhaustion is `unverified`, never a pass.
- Runs are serial, with an exclusive `flock`; the resource caps include children.
  `/usr/bin/time -v` records actual peak RSS. Source archives, process logs and
  output sizes are bounded. Standard test runs use `RUST_TEST_THREADS=2`.

The synthesis planning ceiling is an estimate, not a guarantee about upstream
RTL expansion. CPU benchmark/check children have an 8 GiB address-space limit,
128 MiB per output-file limit and process-group timeouts. The sv2v preparation
step uses an 8 GiB RAM / 1 GiB swap cgroup instead: GHC reserves a large virtual
address range even with a small live heap. Preparation fails if that cgroup is
unavailable. Publication unit tests need less than 64 MiB; their measured peak
was 23 MiB. The release build measured 598 MiB peak RSS.
The positive- and negative-enable latch bindings were separately proved against
Yosys's Verilog/Liberty semantics on both PDKs (81 MiB maximum observed RSS).

## What is compared

- **Complete configured cores**, including register files. VexRiscv is the LiteX
  default RV32IM core with 4 KiB instruction and data caches. All memories are
  synthesized to standard-cell logic/flip-flops; there are no SRAM black boxes.
- **The same prepared RTL and Liberty files** enter both tools. `designs.json`
  pins each upstream commit and configuration. Ibex uses the upstream v2.1.0 tag,
  translated by pinned sv2v. Its generic vendor primitives are bound by changing
  only their module names. The other designs are unmodified upstream Verilog.
  No debug/formal interfaces or FPGA board wrappers are added.
- **One CPU worker**, release RustSys, `--goal area --effort medium`. Yosys uses
  `synth -flatten -noabc -nofsm -nordff`, Liberty flip-flop mapping, then ABC's default
  Liberty mapping. Disabling FSM recoding keeps the original state encoding,
  matching RustSys's state-preserving default. Yosys may still merge redundant
  state. The small latch bindings use only the pinned libraries' public Boolean
  functions; they are included in the measured Yosys flow.
- **Prepared RTL to exported netlist wall time**, not a kernel microbenchmark.
  RustSys includes starting four CLIs, creating an empty workspace, importing
  RTL/Liberty, writing and reading binary tables with mmap, synthesis, and Verilog
  export. Mutations use `--base-rev rev0` and `--base-rev rev1`. Yosys includes
  RTL/Liberty reading, synthesis, mapping, statistics and Verilog export. Both
  timers include the same Python worker startup. Downloads, release compilation,
  common sv2v preparation, and equivalence checking are outside the timed region.
- **One warm-up, three measured repetitions**. Tool order alternates. The median
  wall time is reported, along with the maximum measured RSS. OS file caches are
  warm; every RustSys repetition starts with an empty workspace. All repetitions
  must finish with identical netlist hashes and QoR before a result is accepted.
- **Both PDKs at typical corner**: Sky130 HD `tt_025C_1v80` and GF180 MCU 7T
  `tt_025C_3v30`. `verification/pdks.toml` supplies the commit, file size and
  SHA-256 pin. Cell area is the sum of Liberty cell areas, in square micrometres.
  This is an unconstrained synthesis comparison: no SDC, placement, wiring,
  timing sign-off, power or DRC measurement. All endpoints are unconstrained.

## Validation and reading the table

The table labels every metric pair **RustSys / Yosys**, including separate external LEC
results for the two mapped netlists. The final published matrix has **16/16
complete external proofs**, covering both tools, all four cores and both PDKs.
`lec.py` runs either the open-source
[EQY equivalence checker](https://yosyshq.readthedocs.io/projects/eqy/en/latest/) with SBY,
or an inductive state-transition proof whose Boolean obligations are checked by external
Berkeley ABC CEC. Every measured netlist is compared to the same pinned input RTL
and Liberty. `Proven` requires every EQY-generated partition to have a valid `PASS`,
or every transition/output obligation to pass with complete state, initial-state and
clock/reset coverage. Partial proofs never qualify.

EQY reads canonical RTLIL snapshots, preserving internal latch types and initialization.
RustSys checks use its documented bit-position names; Yosys checks preserve original
Verilog indices, including narrowed vectors, while normalizing `[0:0]` scalar spelling.
The proof interface is split into individual
bits and checked against the original port widths. A common literal alias has one
independent cutpoint; other aliases, including output bits, retain their actual drivers.
Every output remains a proof obligation. Connectivity
metadata is extracted from these same snapshots in a bounded, short-lived process so its
heap is released before EQY starts. Constant and alias correspondence points keep their
drivers; `--prune-aliases` instead removes redundant internal matches when binding would
make partitions too large. `--exclude-internal` records further optional internal matches
to remove after examining a counterexample. These options never remove primary outputs
or alter either design. `--bind-output` can retain additional output cones, such as
PicoRV32's `mem_la_*` interface. Every generated obligation must still pass.
`--amend-output` adds the output's original driver context to consumers without requiring
the same broad partition merge; the producing partition must still be proved separately.
For RustSys, proof ports use a separate recorded namespace so they cannot collide with
literal exported bit-wire names. The mapping is bijective and every original bit is checked.

For gated or derived clocks, `--multiclock` normalizes mapped cell expressions through
Verilog while preserving attributes and initial values, then applies `clk2fflogic` and
requires a combinational-cycle check. Before using a logical latch model, EQY independently
proves it against the pinned Liberty model. This handles GF180's complemented internal
latch state without changing its pins or behavior. Certificates and model hashes are saved.
`--partition-clocks` instead retains clock cutpoints and performs edge sampling after
partitioning; all clock-producing partitions must also pass.
`lec.py`
returns exit status 2 if any selected netlist remains unverified; the complete result
records are still saved.

After EQY creates its partitions, the runner executes its generated strategy Makefile
directly. This avoids repeating both frontends and partition generation. Missing or failed
partition certificates cannot be hidden by a successful `make` exit. `--jobs` controls
independent proof workers (default 1, maximum 4); an outer cgroup bounds their aggregate RAM.
These verification runs remain outside the single-threaded synthesis measurements.
The default budgets are three hours per netlist and ten minutes per strategy/partition.
Arithmetic composition allows four hours in total, with up to one hour for its
initial whole-design CEC attempt. These are configurable work limits, not proof assumptions.
PDR can fall back to SMT induction with `--strategy pdr-smt`. Live output-size checks,
per-process address-space limits and an outer physical-memory cgroup remain required;
exhaustion never becomes a pass.
`--group-bits 32` combines adjacent bits into bounded word-sized obligations, retaining
every assertion while avoiding repeated scalar cones. For large mapped designs,
`--compact-library-wires` removes unused cell-pin aliases introduced by Liberty flattening
while retaining the original top-level names. Multithreaded frontend allocations
may need `--memory-gib 16` for virtual address space, including some Ibex models;
the outer physical-memory cgroup remains independently capped at 4–8 GiB.
Optional `--strategy ric3-pdr-smt --smt-solver bitwuzla` adds independent rIC3 and
Bitwuzla backends when their executables are on `PATH`. Selected solver versions and
hashes are recorded; a missing selected solver yields `Unverified`.
`--strategy ric3-kind-pdr-smt` uses rIC3's k-induction through SBY, followed by
ABC PDR and SMT fallback. It is separate from the disabled native EQY SAT path.

`--canonical-cuts` removes redundant wire names from the proof models while keeping
every port and a recorded set of state matchpoints. An output that directly exposes
a register becomes that register's canonical matchpoint; its internal aliases cannot
become unrelated free inputs. The external frontend performs the wire cleanup, and
the runner verifies that every retained name survives. Original and resulting model
hashes are recorded. Literal output aliases are grouped together. Combinational
output drivers are added to consuming partitions; registered output boundaries remain
independent proof obligations. Metadata work is bounded at one million
named bits, with a 4 GiB subprocess cap; model annotation streams files up to 1 GiB.
This option does not add reset assumptions or change the measured netlist.

For multiclock proofs, SBY strategies use explicit edge sampling. The native EQY
`sat` strategy is disabled: it accepted a deliberately inverted output in a
control for the pinned toolchain. Cached strategy statuses cannot establish a
proof; every partition needs an actual SBY engine `PASS`. The independent control
suite checks aliases, original vector indices, clock feedback, deliberately
bypassed clock gates and output corruptions on both PDKs.

`--proof-mode transition` uses an inductive state-interface proof with external
ABC CEC. The adapter preserves every storage bit and proposes a relation between
the two designs' state variables. It checks that this relation covers every
initial state of the mapped implementation. State without a correspondence is
universally quantified independently, so it cannot be silently assumed away.
The reference uses procedural Verilog priority, without the optional `full_case`
and `parallel_case` synthesis promises. The adapter removes only these attributes
and recognized comment pragmas **before** the external frontend reads the source;
it preserves other attributes, strings, source byte positions and line numbers.
Original and normalized source hashes are recorded. Removing the attributes from
RTLIL alone is insufficient: a frontend may already have replaced an incomplete
case's storage with undefined values. Both synthesis pipelines use this same case
policy. No reset sequence, startup-cycle exclusion or input assumption is added.

The proof compares every original output and the data, clock/latch-enable and
asynchronous-reset functions for the related state. Reset values and storage
kinds are checked as well. If these obligations pass, the relation is preserved
at every event; this is an unbounded proof, with no added reset sequence.
Data functions are compared at active clock/latch events outside asynchronous
reset. Clock and reset functions are checked independently and unconditionally;
original outputs are not masked by storage enables. A separate optimized RTL
copy may propose duplicate-state aliases, and unique `genblkN` names may be
matched across frontends. All original storage bits, initial constraints and
transition functions still participate in the proof; the proposal is not an oracle.
Unmatched named state outputs may be resolved through bounded buffer/inverter
chains. Complemented state relations account for the polarity of data, reset
values and initial values, so library `Q_N` implementations can be compared to
the corresponding RTL register without equating opposite Boolean values.
For bare complementary-output library flops, `library_models.py` preserves one
arbitrary physical state bit. The [Liberty reference manual, ff group](https://zao111222333.github.io/liberty-db/2020.09/reference_manual.html)
defines the two internal variables as the noninverting and inverting outputs
of that storage bit. An external CEC checks the generated model against the
imported cell under this state encoding: all output pins, both next-state rails
and both clock functions. Both physical initial values remain free; no reset or
input constraint is introduced. Unsupported instantiated dual-output models
produce an explicit diagnostic.
An asynchronous reset determines the visible register output while asserted;
its underlying state remains an arbitrary symbolic input. Both reset values
remain possible inputs to the proof. This does not assume a reset sequence.
`--transition-unpair-constant NAME` rejects an optional internal constant-state
correspondence when it is unsuitable. The original state then remains an
independent symbolic input to every observable cone; no output is discarded.

Internal RTL-to-RTL stages can use the `gate_refinement` configuration flag.
The v6 model applies formal X propagation to both sides. Each original output
and next-state bit has a value comparison plus a separate requirement that the
target is defined whenever the reference is defined. This permits partial RTL
intermediates without treating an unknown target as an arbitrary successful
Boolean implementation. Mapped-netlist checks retain the total-function check.
Every stage must pass independently before transitive composition; the checker
verifies shared intermediate hashes, all child certificates, and bounded,
acyclic certificate dependencies. No reset or startup-cycle assumption is added.

`staged_lec.py` can prove difficult word arithmetic through bounded, separately
checked synthesis checkpoints. Stages containing `$macc_v2` or `$alu` use
`native_comb.py`: the state adapter checks initial coverage and all event/state
obligations, then removes every storage cell. Yosys `equiv_induct -undef -seq 1`
proves the resulting purely combinational functions. The adapter rejects any
remaining storage or formal assumptions, verifies the complete port interface,
and requires every comparison point to be proved. Optional internal names are
limited to output cones with a two-million-bit/expansion budget; unused aliases
are discarded as hints. A fresh ports-only retry retains every real obligation.
This path passed all 54 positive/negative semantic controls, including rejection
of a target that loses definedness. The separately archived native RustSys
checker is a different check.

For Ibex's signed multiply-add, `mac_lec.py` connects the original RTL to a
word-level intermediate, then to the measured mapped netlist. `mac_context.py`
extracts the exact 17-bit signed operands, 34-bit addend and all 34 result bits.
[Arisca](https://github.com/LittleBlackCQ/arisca) checks the Boolean cone against
`[17]*[17]+[34] --signed`, modulo the 34-bit result width. The adapter verifies
AIGER input/output indices, rejects hidden inputs, and checks the exact context
replacement: every other cell, register, clock, reset, port and initial condition
is unchanged. The adjoining whole-design proofs must both complete. This is a
compositional proof of every original output and state update, not an arithmetic
test substituted for CPU equivalence.

The zero-completed, uninitialized Boolean intermediate is only a proof witness.
Completing an intermediate alone never establishes original-RTL correctness:
the separate undef-aware proof must establish original RTL to word-model
refinement first. No initial-state or reset assumption is added. Arisca exit
status alone is insufficient; an explicit successful verification verdict is
required. Dual-PDK controls reject wrong low/high result bits, signedness,
operand binding, state and clock connections. The tool runs externally; no
reference implementation source is read, copied or linked into RustSys.

For control functions the word solver cannot close, `partition_comb.py` proves
two disjoint output projections independently. The word proof must completely
close its projection; formal X propagation and ABC close the remaining control
outputs. Both copies retain the exact same cell equations and every input.
Validation checks that the output sets cover every original port and state/event
obligation exactly once. A standalone projection is never accepted as a
whole-design proof. Dual-PDK corruption controls check wrong arithmetic, wrong
control values, loss of definedness, omitted outputs and changed graphs.

This mode rejects undefined clock/reset cones and checks that the implementation
functions are total. Its current `abc-inductive-transition-priority-v5` certificates
use the external frontend's required formal `xprop` encoding to obtain both the
RTL value and its unknown mask. Defined input/state variables have zero unknown
masks. Both CEC sides compare `value & ~RTL_unknown`, so every dynamically defined
RTL bit must match, including state data. This adds no assumptions. Earlier v1
certificates proved a fixed zero completion of RTL data don't-cares under their
recorded case-attribute policy. New proofs use the stricter source policy above;
the experimental v3 model is rejected because its attribute edit occurred too late.
An original output bit that is
literally undefined in the reference has no Boolean constraint; its discharge is
recorded explicitly. Dynamically defined output functions remain obligations.
Before CEC, both AIG interfaces
must contain every expected input/output bit in the same verified order.
Implementation totality is checked in the already loaded, bounded model before
cleanup. This avoids serializing and reloading a second large JSON model. Width
reduction runs before arithmetic Boolean lowering, preventing redundant sign
extensions from inflating the multiplier circuits presented to the solver.

An already Boolean reference has a bounded encoding shortcut. The adapter
requires supported Boolean cells, only binary constants, and exactly one driver
for every used bit; external `scc -expect 0` and `check -assert` must also pass.
Its explicit unknown masks are then zero. The certificate records and hashes
these checks and the original comparison model. Partial operations, undriven
wires and unknown constants retain formal X propagation. This avoids duplicating
large, fully defined gate graphs without adding input-value or reset assumptions.

For difficult multipliers, `arithmetic_lec.py` provides a compositional route.
A bounded diagnostic export records pre-optimization multiplier cones. The
adapter checks each cone against the exported node equations, and verifies the
AIG-to-polynomial encoding and the signed/unsigned integer product specification.
[AMulet 2.2](https://github.com/d-kfmnn/amulet2) produces an algebraic certificate;
[Pacheck 2.0](https://github.com/d-kfmnn/pacheck2) independently checks every
inference and the target. A successful process exit alone is insufficient.
Constant sign-bit specializations also require an external ABC cofactor proof.

Two complete external LECs connect the original RTL to the ideal arithmetic
model, and the unmodified pre-mapping circuit to the measured mapped netlist.
Only independently certified multiplier outputs differ between the two middle
models. The adapter verifies those exact replacements and the shared file hashes
before composing the three proofs. A component proof or an incomplete side LEC
cannot make the CPU `Proven`. No reset sequence or startup-cycle exclusion is used.
`dump_transitions TOP INPUT OUTPUT trace-cells`
uses internal storage cells solely to avoid expanding tens of thousands of
registers into large behavioral processes. The diagnostic files are not timed
synthesis inputs or substitute benchmark netlists.

The Yosys synthesis pipeline uses `-nordff` to retain RTL memory-read register
boundaries. It completes only combinational undefined/undriven bits with zero
before export; it does not set register initial values. Case-promise removal,
frontend processes and this completion are included in the synthesis timer.
ABC's automatic `cec -p` partitioning is disabled after a complemented-output
control exposed a false pass. A whole-design CEC attempt can fall back to explicit
output projections: input ports and the binary AND graph are copied unchanged,
and every output index must be covered by a successful unpartitioned CEC call.
The default is ABC's positional `&cec` engine; `--transition-cec-engine legacy`
selects `cec -n`. Both engines are calibrated with deliberate data, clock and
alias errors. `--transition-batch-outputs` bounds each explicit projection at
1–64 outputs. `--transition-whole-attempt` limits only the initial whole-graph
attempt, before falling back to batches; the shared `--timeout` still applies
to all model preparation and every proof obligation together.
`resume_transition.py` can rerun only CEC after a completed preparation. It
requires the saved state/initial/clock/definedness guards, verifies the original
RTL/netlist/library hashes and both AIG interfaces, and checks the model hashes
again after CEC. It copies the previous attempt into a fresh directory and
preserves the previous verdict. Missing preparation or changed data is rejected.

An explicit `--toolchain-manifest` permits a newer, separately pinned verifier
without changing the synthesis binaries or measurements. The manifest records
versions and hashes of the executable launchers, underlying binaries and EQY
plugins. The large VexRiscv model uses a configurable 1.5-million-cell limit; its adapter runs
under the same 8 GiB physical-memory cgroup and exclusive scale lock. Time and
resource exhaustion remain `Unverified`.
Its roughly 74,000 state bits become bounded transition obligations. JSON models
can approach 650 MiB on disk and several GiB in memory. Gold and gate AIG lowering
therefore run in separate frontend processes, releasing each allocator before
starting the next model. All stages retain the same shared proof deadline.
Internal-cell sources are compacted before their first RTLIL export, retaining
all logic, ports and initialization while removing source-location metadata.
The streaming case-attribute filter permits up to 1 GiB for these generated
models; ordinary source files retain their smaller limit. Internal unpacked
memory declarations are omitted only from the name-adapter metadata. The
functional models still undergo complete structural memory lowering.

For the EQY mode, the relation is its documented
[safe-replacement equivalence](https://yosyshq.readthedocs.io/projects/eqy/en/latest/xprop.html),
with formal X propagation and sampled-edge `async2sync` storage. The scripts add no reset or
initial-state values. Timeouts, failed partitions, unsupported cells and resource exhaustion
remain `Unverified`; a partition counterexample alone is not automatically a whole-design
counterexample. Synthesizer timings and area are unchanged by proof retries.

The native `rustsys check synth` result is recorded separately. An external LEC
certificate never changes an incomplete native check into a pass. Historical
diagnostic runs also use the external Yosys executable to read each mapped netlist and
the input RTL, expands Liberty cell functions, and attempts an equivalence
proof using `equiv_make`, `equiv_simple` and bounded `equiv_induct`. The proof
uses Yosys's sampled-edge `async2sync` model for asynchronous storage. A proof
must contain nonzero proof obligations and end with `equiv_status -assert`;
missing tools, timeouts, unsupported cells and failed proofs are **unverified**.
These conditional induction diagnostics do not replace complete external certificates
and are not sufficient to label a result `Proven` in the current comparison table.
The check is not a physical timing check or a four-state simulation guarantee.

The earlier `equiv.py` flow lowers each side to ordinary Verilog with
`proc; flatten; memory_collect; memory_map; opt_clean; async2sync`. This preserves undefined
memory-read choices: `memory_dff` and other `opt` passes that select a particular don't-care
implementation are excluded from the reference lowering. `write_verilog -norename` keeps
private temporary wires private. The adapter then changes **wire names only** to RustSys's
state-naming contract (SYNTH-REQ-0603): packed bits are
numbered from the least-significant bit, and width-one packed values omit the bit suffix.
This covers vectors such as `[4:2]`, ascending vectors and packed array elements. Ports keep
their public names. A collision or unsupported declaration is explicitly unverified. The
adapter caps lowered files/scripts at 128 MiB, declarations at two million and vector bits at
one million. The same proof deadline covers lowering, adaptation and all proof steps. No
assumptions, black boxes or truth functions are added. `equiv_simple -undef -seq 1` and
`equiv_induct -undef -seq 4` prove **RTL refinement**: where the RTL specifies a defined
value, the netlist must agree; an RTL undefined value may be implemented by either Boolean
choice. This does not independently enforce RustSys's stricter literal-X/Z-as-zero policy
(SYNTH-REQ-0201). The native checker enforces that policy, and `validate_equiv.py` separately
checks it against a fully specified golden fixture. Initial states are unchanged; no
`setundef`, extra reset sequence or power-up assumption is introduced.

`publish.py` attaches selected complete certificates to the current measurements.
It checks the three measured netlist hashes, RTL/PDK hashes, actual external solver
verdicts, coverage and composed arithmetic evidence before rendering the table.
The README combines the four-core campaign and `OpenC910†` stress results in one
table, retaining their distinct sampling and proof scopes in the surrounding notes.
Both publishers update their measured core/PDK rows while preserving the other
campaign's rows; OpenC910's full cell counts remain in its detailed result archive.
Old proofs can be reused only when their actual inputs and netlists have identical
hashes. It archives the selected evidence, and lists larger reproducible intermediate
models by hash. For example:

```sh
python3 verification/benchmarks/open-hardware/publish.py \
  --measurements .work/open-hardware/current \
  --selections .work/selected-proofs.json \
  --output verification/benchmarks/open-hardware/results/current \
  --date YYYY-MM-DD --root-readme README.md
```

To reproduce Ibex's composed proof from a measured run, use the pinned synthesis
and verification binaries from the result's evidence/tool manifests:

```sh
free -h
systemd-run --user --scope -p MemoryMax=4G -p MemorySwapMax=1G \
  timeout 11000s python3 verification/benchmarks/open-hardware/mac_lec.py \
  --config .work/open-hardware/current/ibex/sky130hd_tt/yosys-3/config.json \
  --workspace .work/ibex-lec-sky130 \
  --bindings verification/benchmarks/open-hardware/ibex-mac-bindings.json \
  --synthesis-yosys "$SYNTHESIS_YOSYS" --yosys "$PROOF_YOSYS" \
  --eqy "$EQY" --abc "$ABC" --arisca "$ARISCA" --timeout 10800
```

Repeat with the GF180 measured configuration in a fresh workspace. The runner
captures public CLI checkpoints, proves the original RTL through checkpoint 7,
and checks both adjoining full-design proofs and the MAC replacement. `--prefix`
can reuse a previously completed checkpoint directory; every reused child and
intermediate hash is revalidated. `staged_lec.py --through-stage 7` creates such
a prefix without claiming a proof of the final measured netlist. Archive entries
record exact commands and hashes for the actual selected proofs. Positive and
negative end-to-end controls also exercise a corruption only in the last output.

`--proof-method legacy` reproduces the
original snapshot's unadapted proof; existing proof logs are preserved on retries.

Only a row with successful external proofs for **both** tools and an equivalent
RustSys checker verdict can support a speedup claim. Measured time, memory and
area remain visible for unverified rows, labelled accordingly; a fast result
alone does not establish correctness. A failed synthesis has no performance or
area value. `$scopeinfo` records are Yosys hierarchy metadata, excluded from
gate counts; every other unmapped internal cell rejects the measurement.

## Reproduce

Requirements: Linux, Python 3.11+, `/usr/bin/time`, Git, systemd user scopes for
sv2v, pinned Yosys/ABC and sv2v executables, and the Rust toolchain of this repo.
No reference-tool implementation source is required or inspected.

```sh
free -h
systemd-run --user --scope -p MemoryMax=8G -p MemorySwapMax=2G \
  timeout 600s env CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2 \
  cargo build --release --locked -p rustsys-cli -p rusteda-cli --no-default-features
systemd-run --user --scope -p MemoryMax=8G -p MemorySwapMax=2G \
  timeout 600s env CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2 cargo xtask pdk fetch

BENCH=verification/benchmarks/open-hardware
python3 "$BENCH/run.py" prepare --workspace .work/open-hardware --sv2v /path/to/sv2v
flock .work/scale.lock python3 "$BENCH/run.py" run \
  --workspace .work/open-hardware --yosys /path/to/yosys --label reproduction
python3 "$BENCH/report.py" .work/open-hardware/reproduction/results.json
python3 "$BENCH/validate_bindings.py" --workspace .work/latch-bindings \
  --pdks target/pdk --yosys /path/to/yosys
python3 "$BENCH/validate_equiv.py" --workspace .work/equiv-validation \
  --pdks target/pdk --yosys /path/to/yosys

# EQY, SBY, ABC and their witness tools must come from a compatible installed tool suite.
flock .work/scale.lock systemd-run --user --scope -p MemoryMax=8G -p MemorySwapMax=1G \
  python3 "$BENCH/lec.py" --measurements .work/open-hardware/reproduction \
  --output .work/lec-results --eqy /path/to/eqy --yosys /path/to/yosys \
  --abc /path/to/yosys-abc --timeout 10800 --memory-gib 8 \
  --partition-timeout 600 --strategy pdr-smt --bind-output 'mem_la_*'

# Retry one large netlist with the inductive transition flow (three-hour budget).
# The 16 GiB virtual-address allowance does not raise the 8 GiB physical cap.
flock .work/scale.lock systemd-run --user --scope -p MemoryMax=8G -p MemorySwapMax=1G \
  timeout 10920s /usr/bin/time -v python3 "$BENCH/lec.py" \
  --measurements .work/open-hardware/reproduction --output .work/lec-transition \
  --design vexriscv-verilog --library sky130hd_tt --tool yosys \
  --proof-mode transition --timeout 10800 --memory-gib 16 \
  --transition-whole-attempt 60 --transition-batch-outputs 64 \
  --toolchain-manifest /path/to/verification-manifest.json \
  --yosys /path/to/yosys --eqy /path/to/eqy --abc /path/to/yosys-abc

# Resume the final CEC stage without rebuilding an unchanged, verified model.
systemd-run --user --scope -p MemoryMax=4G -p MemorySwapMax=1G \
  timeout 1860s /usr/bin/time -v python3 "$BENCH/resume_transition.py" \
  --prepared .work/lec-transition/vexriscv-verilog/sky130hd_tt/yosys \
  --output .work/lec-resumed/vexriscv-verilog/sky130hd_tt/yosys \
  --abc /path/to/yosys-abc --timeout 1800 --engine gia

# Validate aliases, clock feedback, and planted output errors on both PDKs.
systemd-run --user --scope -p MemoryMax=2G -p MemorySwapMax=512M \
  timeout 900s python3 "$BENCH/validate_lec.py" \
  --workspace .work/lec-controls --pdks target/pdk \
  --yosys /path/to/yosys --eqy /path/to/eqy --abc /path/to/yosys-abc

(ulimit -v 262144; timeout 20s python3 -m unittest discover -s "$BENCH" -p 'test_*.py')
```

The runner refuses a changed prepared input, wrong Liberty checksum, unexpected
tool version, unsafe output label, or an existing result directory. It records
versions, a SHA-256 of the RustSys source snapshot (including Cargo.lock), binary
hashes, host details, exact command argument arrays, all repetition measurements,
QoR and proof statuses. Each benchmark workspace has a lock. `verify` can finish
missing proofs from existing measured artifacts without rerunning synthesis.

Use `verify --retry-unverified --proof-method canonical --proof-label <fresh-label>` to retry
an unverified proof. `--design`, `--library` and `--tool` restrict verification. Each proof label
creates new logs and refuses to overwrite an earlier attempt; prior verdicts remain in the
result history. `validate_equiv.py` checks positive vector/array cases and a planted output
error on both PDKs, including the native checker with optional sweeping disabled. It also
distinguishes an allowed RTL-X implementation choice from a violation of the native literal
zero policy, using an independent fully specified golden fixture and a planted error.

To compare checker builds on the same saved `rev2` workspaces, retain the old executable and
run the paired profiler. Workspace cloning is outside the timer; each timed command includes
mmap reading, model construction, all final proofs and writing the report revision. Runs are
serial and tool order alternates; source files, netlists and binaries are hashed.

```sh
free -h
flock .work/scale.lock python3 "$BENCH/profile_check.py" \
  --measurements .work/open-hardware/reproduction \
  --before /path/to/baseline/rustsys --after target/release/rustsys \
  --output .work/checker-comparison --repetitions 3

# Native checker retry, on copies: 30 minutes per corner, all final proofs retained.
flock .work/scale.lock systemd-run --user --scope -p MemoryMax=4G -p MemorySwapMax=1G \
  timeout 3720s /usr/bin/time -v python3 "$BENCH/check_native.py" \
  --measurements .work/open-hardware/reproduction --rustsys target/release/rustsys \
  --output .work/native-recheck --design vexriscv-verilog \
  --timeout 1800 --budget 100000 --sweep-max-candidates 0
```

RustSys had no Git commit in this local checkout; the report therefore identifies
the tested source by its SHA-256, not an invented Git revision.

## Sources and licences

| Design | Source | Licence |
|---|---|---|
| Ibex | [lowRISC/ibex, v2.1.0](https://github.com/lowRISC/ibex/tree/38c070939183cc10940b66c8e9e04eeca6b65470) | Apache-2.0 |
| PicoRV32 | [YosysHQ/picorv32](https://github.com/YosysHQ/picorv32/tree/ef203c2b0a3fb793280f5114941416c425c5b461) | ISC |
| SERV | [olofk/serv](https://github.com/olofk/serv/tree/f200eb2ed7b69ac1c6b8eddd47654522aeee5ce8) | ISC |
| VexRiscv | [LiteX's published Verilog](https://github.com/litex-hub/pythondata-cpu-vexriscv/tree/642ecfed1c84460555d6d803d660cc60cfc1ecb6), generated from [SpinalHDL/VexRiscv](https://github.com/SpinalHDL/VexRiscv) | MIT |

Upstream source trees and generated netlists stay in ignored `.work/`; they are
not copied into RustSys's implementation or synthetic unit-test corpus.

## Recorded results

The [current unified campaign](results/2026-10-21-default-multicore/README.md)
contains all forty RustSys/Yosys design, PDK and host-core configurations, with
three-run measurements, complete arbitrary-state LEC, OpenSTA receipts and source pins.

Two earlier datasets remain because `scaling_verify.py` uses their input bindings
and complete certificates as verification baselines:
[four-core RTL proofs](results/2026-10-20-lec-complete/README.md) and
[calibrated OpenC910 proofs](results/2026-10-20-openc910-verified/README.md).
Their measurement profiles and proof scopes remain separate from the current table.
Superseded intermediate campaigns have been removed. The
[invalidation registry](results/proof-invalidations.json) retains rejected producer
hashes so that removing an old campaign cannot make its certificates valid again.
