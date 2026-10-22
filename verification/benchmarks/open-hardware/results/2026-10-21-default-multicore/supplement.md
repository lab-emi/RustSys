# Source, acceptance and optimization evidence

The primary comparison contains **40 actually measured configurations**: five designs,
two PDKs, both tools at one and sixteen physical host cores. Every configuration has
one excluded warm-up and three measured samples. The Yosys one-core results are new
measurements; the unchanged Yosys sixteen-core references retain their earlier campaign.
All measured tool versions, commands, inputs and source-campaign hashes are retained.

## Acceptance

[Machine-readable acceptance](acceptance.json) checks the complete matrix against the
previous RustSys campaign and the pinned Yosys reference:

- RustSys is faster in all twenty same-core-count time comparisons.
- All ten RustSys results have smaller area and a higher setup-Fmax estimate than Yosys.
- No RustSys area or setup-Fmax regression relative to the previous measured RustSys build.
- Each tool's one-core and sixteen-core netlists are byte-identical, so quality and full
  arbitrary-state LEC bind to both allocations.
- All twenty distinct netlists have complete LEC and numeric OpenSTA evidence; reset and
  input assumptions are `none`.

Fmax uses the documented ideal-clock, zero-wire model. Default cleanup can leave electrical
violations, and the full-core OpenC910 area-mode paths are outside Liberty load ranges.
This is not physical timing closure or a universal PPA claim.

## Implementation and controls

The changes add bounded parallel balance planning, bounded exact mapping plans with validated
reference-count updates, compact intermediate cover counts, and ordered parallel Verilog
formatting/export. Default cleanup spends recovered area on load-aware repair under the
starting cover's area ceiling. Its optional alternate cover is driven by measured load
violations and graph budgets, without design-name or PDK-cell-name conditions.

[Checks](final-checks.json) include 101 engine tests, 26 CLI tests, four writer/export tests,
two real-PDK fixture exports and 30 harness checks. CUDA-only validation is separately marked
ignored on this CPU benchmark host. All-target/all-feature Clippy and formatting checks passed.

[optimization-controls.json.gz](optimization-controls.json.gz) contains **155 complete external ABC CEC checks** of pre/post
optimization roots, with state and event inputs arbitrary, plus six real-PDK sequential
controls. The correct Sky130/GF180 fixtures pass; deliberately broken enable/clock cases
produce external counterexamples. Exporting a model alone is never accepted as a proof.
Raw small models, solver receipts and bounded build/test RSS records are in
[optimization-evidence.tar.gz](optimization-evidence.tar.gz); larger reproducible files are pinned in
[optimization-large-artifacts.json.gz](optimization-large-artifacts.json.gz).

## Exact measured producer

The [source snapshot](final-sources.tar.gz.index.json) and [producer manifest](final-binaries.json) describe the
exact frozen RustSys/rusteda binaries measured in this campaign. Runtime source files and
binary hashes were rechecked at acceptance. No post-measurement algorithm change is substituted
for the measured producer. The independent LEC tools retain their own pinned executable and
source provenance in the proof records.

The source archive uses ordered parts when larger than 1 MiB. Its index records the combined
SHA-256. Concatenate the listed parts in order before decompressing. The executable files
remain in the local measurement workspace; their hashes, compiler version and build command
are recorded in the producer manifest.

Proof and timing reuse is explicit: complete receipts are transported only when the actual
measured netlist, RTL, library and applicable tool/constraint hashes match. This is not reported
as a fresh solver or STA run. New changed small-design netlists were proved during this iteration;
unchanged full-core netlists retain their complete original event/root certificates.
