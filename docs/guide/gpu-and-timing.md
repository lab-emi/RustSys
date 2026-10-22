# GPU mapping and timing refinement

CPU synthesis is the default. GPU mapping accelerates selected mapping passes, and timing refinement adds a load-aware implementation estimate. Each is an explicit choice; ordinary synthesis needs neither.

## Select a mapping backend

```sh
rustsys --version
rustsys synth --backend cuda --lib cells.lib \
  --out .work/top_cuda.v --report .work/top_cuda.json top.sv
```

`--backend cpu` uses the pure-Rust implementation. `cuda` requests native CUDA kernels on NVIDIA GPUs. `gpu` selects the first usable GPU runtime in the build after device preparation and a self-test. The current runtime is CUDA; Metal is planned.

The CUDA runtime needs a compatible driver and CUDA toolkit at runtime. Set `CUDA_PATH` to the toolkit root, and make its libraries, including NVRTC, visible to the dynamic loader. A missing or failing requested runtime is an error (`S0530`), rather than an implicit change of backend.

The delay and area-flow cut passes can run on the GPU; exact area recovery remains on the CPU. The supported backends produce byte-identical mapped netlists for the same inputs and options. `map_backend` records the runtime that actually ran, and `map_phase_s` records mapping phase timings. This does not promise a speedup on every design: include device preparation, lowering, transfer and synchronization in end-to-end measurements.

## Add load-aware refinement

```sh
rustsys synth --top chip_top --lib cells.lib \
  --timing-period 2.0 \
  --input-delay 0.1 --output-delay 0.1 \
  --input-slew 0.1 --clock-slew 0.1 \
  --output-load 0.01 --timing-margin 0.05 \
  --buffer-fanout 8 --area-limit 10000 \
  --out .work/chip_timed.v --report .work/chip_timed.json chip_top.sv
```

`--timing-period` enables ideal-clock load-aware refinement using Liberty NLDM tables. RustSys can resize cells, buffer loads and refine the mapped cover, subject to the requested timing and area targets. The timing arguments above use nanoseconds; output load uses picofarads. Area uses the library's area units.

The report's `electrical` object states whether the targets were met and records unresolved violations. Missing area, estimated setup, or electrical limits emits a warning (`S0532`, `S0533`, `S0534`) and can still produce a netlist with exit 0. Gate acceptance on those reported results if your flow requires them.

Additional electrical corners use the same cells, pins, functions and areas:

```sh
rustsys synth --lib typical.lib --electrical-corner slow.lib \
  --electrical-corner fast.lib --timing-period 2.0 \
  --out .work/top_multi.v --report .work/top_multi.json top.sv
```

These corners contribute capacitance and transition limits. This is not a complete multi-corner setup/hold signoff analysis. The estimate uses ideal clocks and no routed interconnect; run downstream static timing analysis with the actual constraints and parasitics.

## Refine a committed workspace

Workspace mode can resize selected instances or run bounded incremental optimization:

```sh
rustsys -w .work/design --format json opt-timing --base-rev rev2 \
  --max-iters 16 --max-area-growth-pct 5 --request-id timing_trial_1

rustsys -w .work/design --format json resize --base-rev rev3 \
  --target inst12 --drive-multiplier 2 --request-id resize_trial_1
```

The revision numbers and `inst12` are illustrative: query the actual current revision and valid target handles first. `--request-id` is an idempotency key for these mutations. Keep the resulting revision and compare it with `rusteda diff`; check equivalence for the final implementation rather than assuming a refinement command establishes it.

See [electrical requirements](../specs/synth/electrical.md), [compute runtimes](../specs/compute/README.md), and the [full synthesis manual](../user/synth/README.md).
