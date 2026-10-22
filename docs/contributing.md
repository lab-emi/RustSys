# Contribute to RustSys

Changes should be easy to review, reproduce and verify. Start with the tracked [repository rules](https://github.com/lab-emi/RustSys/blob/main/CONTRIBUTING.md), then identify the requirement and evidence affected by your change.

## Scope a change

For a new behavior, write or update its requirement in `docs/specs/` and cite that requirement ID in implementation and tests. Keep synthesis and checker implementations independent. A logic optimization needs equivalence evidence; a performance change needs end-to-end runtime, peak memory and netlist-quality evidence together.

Reference EDA tools are black-box executables communicating over standard formats. Do not inspect, port, copy or translate their implementation source to close a difference. Resolve semantics against the specification and independent observations. Core and engine crates remain free of Python, ML and GUI runtime dependencies.

Use synthetic redistributable design fixtures. PDKs and upstream hardware inputs are fetched at pinned revisions/checksums by the verification tooling rather than copied into ordinary test directories. Keep scratch outputs below the ignored `.work/` directory.

## Run checks appropriate to the change

The normal repository checks are:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo xtask layout
cargo xtask shared check
cargo xtask coverage
cargo xtask fixtures
cargo test --workspace
cargo xtask verify synth
cargo deny check
reuse lint
```

Review regenerated fixtures and coverage before accepting them. Changes to shared files also require `cargo xtask shared update` followed by `cargo xtask shared check`. CUDA work adds `cargo xtask verify synth --gpu` on a configured GPU host. Liberty, mapping or timing changes need the dual-library verification flow, `cargo xtask verify pdk --require`, with the intended PDKs installed.

These checks include heavyweight builds and suites. Estimate their peak memory first; inspect available host RAM, limit build/test concurrency, and set explicit memory and wall-clock guards. For example, set `CARGO_BUILD_JOBS=2` and `RUST_TEST_THREADS=2` deliberately on a shared host, then use suitable cgroup limits for the selected suite. Serialize jobs expected to exceed 4 GiB behind an exclusive `.work/scale.lock`. Measure actual peak RSS with GNU time.

Standard unit/integration test binaries must stay within 2 GiB RSS, and PDK verification runs within 8 GiB RSS. Large-design stress benchmarks have separately disclosed resource profiles; those profiles do not relax ordinary test limits. A killed process or missing reference executable is incomplete evidence.

The tracked CI and verification suite definitions specify the exact checks for each change. Documentation-only work should build the site strictly and validate examples; it does not require repeating unrelated full synthesis campaigns. See [maintaining the documentation](about/maintaining-docs.md).

## Prepare a review

Describe the concrete behavior change, its requirement, relevant resource bounds and independent validation. For benchmarks, include the design configuration, executable pins, end-to-end timing interval, QoR, peak RSS and proof scope. Never convert unchecked status into a zero metric or passing claim.

Use Conventional Commit subjects and sign off contributions under the Developer Certificate of Origin. Follow the [code of conduct](https://github.com/lab-emi/RustSys/blob/main/CODE_OF_CONDUCT.md). Security reports belong in the [private disclosure process](https://github.com/lab-emi/RustSys/blob/main/SECURITY.md).
