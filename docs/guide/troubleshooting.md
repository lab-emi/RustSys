# Troubleshoot a run

Start with the exit code and the first coded diagnostic. Keep the original inputs, report and command so that a failed run can be reproduced.

| Symptom | Meaning and next step |
|---|---|
| `A0003`, unknown option | Use `rustsys <command> --help`. There is no `runs` command; use `show`. |
| Top module not found | Check `--top`, source files, package order and preprocessing definitions. |
| Unsupported construct | Read the [language boundary](../reference/language-support.md); do not remove semantics merely to make a parser accept the file. |
| `S0530`, unavailable backend | Configure the requested CUDA runtime or deliberately rerun with `--backend cpu`. |
| `S0532` / `S0533` / `S0534` | Inspect unmet area, estimated timing or electrical targets in the `electrical` report. |
| `C0005`, `undecided`, exit 5 | A named proof/search reached its budget. Increase that budget or partition the verification flow while retaining complete obligation coverage. |
| `not-equivalent`, exit 3 | Inspect mismatches, state correspondence, exact libraries and semantic options; replay counterexamples. |
| `REVISION_CONFLICT`, exit 4 | Another commit changed workspace HEAD; reload it before choosing a new base. |
| Run directory already used | Choose a fresh `--run-dir` or use `--record` under a runs root. |

## Resource limits

Estimate memory before a large run, including graph nodes, cut tables, proof clauses and worker concurrency. Measure peak RSS instead of assuming a timeout will prevent an out-of-memory failure. Run high-memory jobs serially; keep independent low-memory jobs concurrent only when the host has room.

For a small pure-CPU experiment on Linux, the following shell example caps virtual memory at 1 GiB and wall time at 120 seconds:

```sh
(
  ulimit -v 1048576
  /usr/bin/time -v timeout 120s rustsys --threads 1 synth \
    --top counter \
    --lib verification/fixtures/synthetic/synth/lib/wt_rich.lib \
    --out .work/counter.v \
    verification/fixtures/synthetic/synth/rtl/counter/counter.sv
)
```

Choose limits appropriate to the design. A virtual-memory cap is unsuitable for GPU runs that reserve large address spaces; use a cgroup memory/swap guard where available. A cgroup also confines failure to the intended child process. A command killed by a wall-clock or memory guard has not completed synthesis or proof.

The native checker's `--budget` and `--model-budget` count SAT conflicts per proof. Raising the wall-clock timeout alone does not raise those budgets. Conversely, raising conflict budgets does not remove the need for an OS memory cap.

## When equivalence cannot be established

First confirm identical RTL configuration, library models, ports and exported bytes. Distinguish a counterexample from an unsupported model and from exhausted work. Record `undecided` or `unverified` until every obligation is proved; a missing external tool also leaves verification incomplete.

Do not introduce a reset assumption to make an arbitrary-initial-state proof pass. Reset-constrained behavior is a separate contract that must be requested and labeled explicitly. See [checking](checking.md).

## Report a reproducible issue

Include the executable version, command, operating system, relevant diagnostics, report, resource limits and the smallest redistributable reproducer. For a recorded run, `rustsys show RUN --section command` prints the canonical command. Avoid publishing proprietary RTL or licensed PDK data. See [contributing](../contributing.md) for the engineering and verification workflow.
