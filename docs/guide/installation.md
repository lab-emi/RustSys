# Install RustSys

RustSys 0.1.0 is unreleased and is not published on crates.io. Install from this repository with Rust **1.97.1** and Cargo. The CPU workflow needs no external synthesis program, Python runtime or GPU toolkit.

## Build from source

Use RustEDA's pinned recursive checkout, or clone the integration branch directly
(the standalone `main` branch can lag this documentation):

```sh
git clone --branch codex/unified-brand https://github.com/lab-emi/RustSys.git
cd RustSys
```

From the repository root:

```sh
cargo install --locked --path crates/apps/rustsys-cli
rustsys --version
rustsys synth --help
```

The default build includes the CUDA runtime, but ordinary `--backend cpu` synthesis does not require a GPU. For a CPU-only build:

```sh
cargo install --locked --path crates/apps/rustsys-cli --no-default-features
```

Install the companion workspace command if you want immutable revisions, library caching and JSON envelopes:

```sh
cargo install --locked --path crates/apps/rusteda-cli
rusteda --version
```

Cargo installs the binaries into its configured `bin` directory. Ensure that directory is on `PATH`. For development without installation, build with `cargo build --release -p rustsys-cli -p rusteda-cli` and use `target/release/rustsys` and `target/release/rusteda`.

## What you need for a real design

Supply your synthesizable RTL, its include paths and parameters, a top module, and at least one Liberty library. RustSys emits a flat structural Verilog netlist that instantiates cells from that library. It does not supply a production PDK with the executable.

The [quickstart](quickstart.md) uses a small synthetic library included in the repository. That library is useful for learning and regression tests; use the intended technology library for implementation work.

## Platforms and resources

Linux is exercised in CI. Builds on other platforms have not received the same validation. Native CUDA mapping is available; a Metal runtime is planned. See [GPU and timing workflows](gpu-and-timing.md) before selecting a non-default backend.

Start with `--threads 1` when measuring or integrating a new design. The default is one worker per available CPU. Large designs also need explicit process memory and wall-clock limits; see [resource guidance](troubleshooting.md#resource-limits).

Continue with [your first netlist](quickstart.md), or use the [agent workflow](agents.md) to build a revision-aware integration.
