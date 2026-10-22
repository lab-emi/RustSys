<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSys contributors -->

# Maintain the documentation

Run these commands from the repository root. Python is needed to build the
documentation, not to build or run RustSys.

## Install the site tools

Python 3.10 or newer is supported by the pinned documentation dependencies.

```bash
python3 -m venv .work/docs-venv
.work/docs-venv/bin/python -m pip install -r docs/requirements.txt
```

With `uv`, the equivalent setup is:

```bash
uv venv --python 3.13 .work/docs-venv
uv pip install --python .work/docs-venv/bin/python -r docs/requirements.txt
```

`docs/requirements.in` pins the three direct tools; `docs/requirements.txt`
locks their transitive dependencies. Refresh the lock deliberately with
`uv pip compile docs/requirements.in --output-file docs/requirements.txt`.

## Build and check

```bash
.work/docs-venv/bin/python docs/_tools/check_markdown.py
.work/docs-venv/bin/python -m mkdocs build --strict -f docs/mkdocs.yml
.work/docs-venv/bin/python docs/_tools/check_site.py .work/docs-site
```

The generated site is `.work/docs-site/`; it is an ignored build artifact.
The source check validates repository-relative Markdown links, including the
root README. The strict build rejects broken site links and missing navigation pages.
The final check resolves local HTML links, images, scripts, and fragment
targets in the built output. It does not contact remote websites.

For a local preview, run the following command and stop it when finished:

```bash
.work/docs-venv/bin/python -m mkdocs serve -f docs/mkdocs.yml --dev-addr 127.0.0.1:8000
```

The preview URL includes `/RustSys/`, matching the configured GitHub Pages
path. A local documentation preview does not start a RustEDA service or change
the CLI's daemonless execution model.

## Where to edit

Add a task walkthrough in `docs/guide/`, a lookup page in `docs/reference/`,
or a normative requirement in the existing `docs/specs/` tree. Add new primary
pages to `nav` in `docs/mkdocs.yml`. Link with relative Markdown paths so the same
guide works on GitHub and in the site. Preserve existing source paths when
reorganizing the navigation.

Run executable examples against the current CLI before changing a workflow.
Make example workspaces and output paths explicit. Distinguish a documented
command contract from illustrative output, and keep budgets and unsupported
semantics visible.

Keep the product introduction and comparison table in the root `README.md`,
walkthroughs in `docs/guide/`, command summaries in `docs/reference/`, and exact
behavior in `docs/specs/`. Benchmark measurements, proof receipts and source pins
belong in `verification/benchmarks/open-hardware/results/`. Shared RustEDA marks
and the RustSys animation live in `docs/assets/`; the [brand guide](../brand/README.md)
documents their use.

Benchmark values and proof status must be updated from the tracked result
records. Retain the README's `open-hardware-benchmark` and
`large-hardware-benchmark` marker pairs because the benchmark report tools
replace their contents. The common multicore layout also keeps the
`parallel-hardware-scaling` marker pair and is published by `scaling_report.py`;
legacy campaign publishers must not overwrite its compact 16-core overview or mix
tool profiles. The dated archive retains the full single-core/multicore and quality
table. Keep the README, homepage and benchmark guide's compact tables in sync using
`scaling_report.comparison_table` and the same archived measurements and certificates.
Do not revive a certificate listed in the historical
proof invalidation registry.

## Publish to GitHub Pages

The `docs` GitHub Actions workflow builds and validates pull requests and
changes to `main`, then uploads a downloadable site artifact. Successful
documentation updates on `main` deploy automatically to GitHub Pages. Pull
requests only build and validate the site.

For a manual deployment, start the workflow on `main` with **Publish to GitHub
Pages** enabled. Leaving that option disabled builds and validates without
deploying. The repository's Pages publishing source is **GitHub Actions**;
the deployment job uses the `github-pages` environment and scoped Pages/OIDC
permissions.

The online documentation is [lab-emi.github.io/RustSys](https://lab-emi.github.io/RustSys/).
Confirm a successful deployment job and check the live URL after publishing.
