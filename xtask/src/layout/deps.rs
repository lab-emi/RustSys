// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Dependency edges between the workspace crates (AGENTS 6.2, 9.2) and of the shared crates of the
//! RustEDA tools.
//!
//! By group: `core` ← `formats` ← `engines` ← `gui`, `apps`; `verify` uses only `core` and
//! `formats` (other verification crates only as development dependencies); engines never use one
//! another; nothing depends on `apps`. Development dependencies of `core`, `formats` and `engines`
//! may also name verification crates (fixtures, defects). Crates below `gui` and `apps` use no
//! user-interface crate (by name prefix, so that a toolkit's companion crates count too).
//!
//! The shared crates form their own layer below the tools ([`SHARED_DEPS`]): of the workspace
//! crates, `rusteda-core` uses none, `rusteda-workspace`, `rusteda-formats` and `rusteda-compute`
//! use only `rusteda-core`, and `rusteda-cli` uses only shared crates, in every kind of
//! dependency. GPU bindings ([`BACKEND_PREFIXES`]) are dependencies of `rusteda-compute` only.

use super::Report;
use super::manifest::{self, Kind};
use std::collections::BTreeMap;
use std::path::Path;

/// Name prefixes of user-interface crates (a prefix matches the name itself and names continuing
/// with `-` or `_`): only `gui` and `apps` crates may use them.
const UI_PREFIXES: [&str; 8] = [
    "egui",
    "eframe",
    "epaint",
    "ecolor",
    "emath",
    "winit",
    "ratatui",
    "crossterm",
];

/// Name prefixes of GPU binding crates: only [`COMPUTE`] may use them.
const BACKEND_PREFIXES: [&str; 5] = ["cudarc", "objc2", "cubecl", "wgpu", "metal"];

/// The shared crate that holds every GPU binding.
const COMPUTE: &str = "rusteda-compute";

/// The workspace crates each shared crate may depend on.
const SHARED_DEPS: [(&str, &[&str]); 5] = [
    ("rusteda-core", &[]),
    ("rusteda-workspace", &["rusteda-core"]),
    ("rusteda-compute", &["rusteda-core"]),
    ("rusteda-formats", &["rusteda-core"]),
    (
        "rusteda-cli",
        &[
            "rusteda-core",
            "rusteda-workspace",
            "rusteda-compute",
            "rusteda-formats",
        ],
    ),
];

/// True when `name` is `prefix` or continues it with `-` or `_`.
fn has_prefix(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['-', '_']))
}

fn is_ui(name: &str) -> bool {
    UI_PREFIXES.iter().any(|p| has_prefix(name, p))
}

fn is_gpu_binding(name: &str) -> bool {
    BACKEND_PREFIXES.iter().any(|p| has_prefix(name, p))
}

/// A workspace crate: its manifest path and group.
struct Member {
    manifest: String,
    group: String,
}

/// Groups a crate of `group` may depend on through a dependency of `kind`.
fn allowed_groups(group: &str, kind: Kind) -> &'static [&'static str] {
    let dev = kind == Kind::Dev;
    match group {
        "core" if dev => &["core", "formats", "verify"],
        "core" => &["core"],
        "formats" if dev => &["core", "formats", "verify"],
        "formats" => &["core", "formats"],
        "engines" if dev => &["core", "formats", "verify"],
        "engines" => &["core", "formats"],
        "verify" if dev => &["core", "formats", "verify"],
        "verify" => &["core", "formats"],
        "gui" | "apps" => &["core", "formats", "engines", "verify", "gui"],
        _ => &[],
    }
}

/// Checks every crate manifest in `files`; `[workspace.dependencies]` renames of the root manifest
/// apply to members that inherit a dependency. GPU bindings are looked for in every manifest.
pub(super) fn check(root: &Path, files: &[String], r: &mut Report) {
    let ws = manifest::workspace_deps(root);
    let mut members: BTreeMap<String, Member> = BTreeMap::new();
    let mut parsed = Vec::new();
    for f in files {
        let parts: Vec<&str> = f.split('/').collect();
        if f != "Cargo.toml" && !f.ends_with("/Cargo.toml") {
            continue;
        }
        let m = manifest::load_member(root, f, &ws);
        let group = match parts.as_slice() {
            ["crates", group, _, "Cargo.toml"] => group.to_string(),
            // Other manifests (the workspace root, `xtask`) are checked for GPU bindings only.
            _ => String::new(),
        };
        if !group.is_empty()
            && let Some(name) = &m.name
        {
            members.insert(
                name.clone(),
                Member {
                    manifest: f.clone(),
                    group: group.clone(),
                },
            );
        }
        parsed.push((f.clone(), group, m));
    }
    for (f, group, m) in &parsed {
        let me = m.name.as_deref().unwrap_or(f);
        let shared = SHARED_DEPS.iter().find(|(n, _)| *n == me);
        for d in &m.deps {
            let q = d.package.as_str();
            let at = format!("{f}:{} [{}]", d.line, d.kind.section());
            let mut deny = |why: &str| {
                r.errors
                    .push(format!("{at}: {me} must not depend on {q} ({why})"));
            };
            if me != COMPUTE && is_gpu_binding(q) {
                deny("GPU bindings are used only by rusteda-compute, RustEDA contract 1");
                continue;
            }
            if group.is_empty() {
                continue;
            }
            if group != "gui" && group != "apps" && is_ui(q) {
                deny("user-interface crates are used only by gui and apps crates, AGENTS 9.2");
                continue;
            }
            let Some(t) = members.get(q) else {
                continue;
            };
            if t.group == "apps" {
                deny("nothing depends on an apps crate, AGENTS 6.2");
            } else if let Some((_, ok)) = shared
                && !ok.contains(&q)
            {
                deny(&if ok.is_empty() {
                    format!("{me} uses no other workspace crate, RustEDA contract 1")
                } else {
                    format!(
                        "of the workspace crates, {me} uses only {}, RustEDA contract 1",
                        ok.join(", ")
                    )
                });
            } else if group == "engines" && t.group == "engines" {
                deny("engines never depend on another engine, AGENTS 6.2");
            } else if group == "verify" && t.group == "engines" {
                deny("verification crates never depend on an engine, AGENTS 7.1");
            } else if !allowed_groups(group, d.kind).contains(&t.group.as_str()) {
                deny(&format!(
                    "a {group} crate does not depend on the {} group ({}), AGENTS 6.2",
                    t.group, t.manifest
                ));
            }
        }
    }
    for f in files {
        if f.starts_with("crates/verify/")
            && f.ends_with("-check/src/lib.rs")
            && let Ok(text) = std::fs::read_to_string(root.join(f))
            && !text.contains("#![forbid(unsafe_code)]")
        {
            r.errors.push(format!(
                "{f}: checker crate must carry #![forbid(unsafe_code)], AGENTS 7.1"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Tree, manifest};

    /// A tree with the crates of the workspace, the shared crates that come later, a second engine
    /// and the given extra dependency lines.
    fn tree(tag: &str, extra: &[(&str, &str, &str)]) -> Tree {
        let t = Tree::new(tag);
        let crates = [
            ("core/rusteda-core", "rusteda-core"),
            ("core/rusteda-workspace", "rusteda-workspace"),
            ("core/rusteda-compute", "rusteda-compute"),
            ("formats/rusteda-formats", "rusteda-formats"),
            ("engines/rustsys", "rustsys"),
            ("engines/demo", "rustsys-demo"),
            ("verify/rustsys-check", "rustsys-check"),
            ("verify/rustsys-defects", "rustsys-defects"),
            ("verify/rustsys-fixtures", "rustsys-fixtures"),
            ("gui/view", "rustsys-view"),
            ("apps/rustsys-cli", "rustsys-cli"),
            ("apps/rusteda-cli", "rusteda-cli"),
        ];
        for (dir, name) in crates {
            let mut rest = String::new();
            for (who, section, line) in extra {
                if *who == name {
                    rest.push_str(&format!("[{section}]\n{line}\n\n"));
                }
            }
            t.file(&format!("crates/{dir}/Cargo.toml"), &manifest(name, &rest));
        }
        let mut rest = String::new();
        for (who, section, line) in extra {
            if *who == "xtask" {
                rest.push_str(&format!("[{section}]\n{line}\n\n"));
            }
        }
        t.file("xtask/Cargo.toml", &manifest("xtask", &rest));
        t
    }

    fn errors(t: &Tree) -> Vec<String> {
        let mut r = Report::default();
        check(t.root(), &t.files(), &mut r);
        r.errors
    }

    // AGENTS 6.2, 7.1, 9.2 and the RustEDA contract: the edges of the workspace pass, and those of
    // the shared crates that come later.
    #[test]
    fn allowed_edges() {
        let t = tree(
            "deps-ok",
            &[
                (
                    "rusteda-compute",
                    "dependencies",
                    "cudarc = { workspace = true, optional = true }\nobjc2-metal = { version = \"0.3\", optional = true }\nrusteda-core.workspace = true",
                ),
                (
                    "rusteda-formats",
                    "dependencies",
                    "rusteda-core.workspace = true\nthiserror.workspace = true",
                ),
                (
                    "rusteda-workspace",
                    "dependencies",
                    "rusteda-core.workspace = true\nmemmap2.workspace = true",
                ),
                (
                    "rusteda-cli",
                    "dependencies",
                    "rusteda-core.workspace = true\nrusteda-workspace.workspace = true\nrusteda-formats.workspace = true\nclap.workspace = true\ncrossterm.workspace = true",
                ),
                (
                    "rustsys-fixtures",
                    "dev-dependencies",
                    "rusteda-formats.workspace = true",
                ),
                (
                    "rustsys",
                    "dependencies",
                    "rusteda-core.workspace = true\nrusteda-formats.workspace = true\nrusteda-compute.workspace = true\nrayon.workspace = true",
                ),
                (
                    "rustsys",
                    "dev-dependencies",
                    "rustsys-fixtures.workspace = true",
                ),
                (
                    "rustsys-check",
                    "dependencies",
                    "rusteda-core.workspace = true\nrusteda-formats.workspace = true\nserde_json = \"1\"",
                ),
                (
                    "rustsys-check",
                    "dev-dependencies",
                    "rustsys-fixtures.workspace = true\nrustsys-defects.workspace = true",
                ),
                (
                    "rustsys-defects",
                    "dependencies",
                    "rusteda-formats.workspace = true",
                ),
                (
                    "rustsys-view",
                    "dependencies",
                    "rusteda-core.workspace = true\nrustsys.workspace = true\nrustsys-check.workspace = true\negui = \"1\"",
                ),
                (
                    "rustsys-cli",
                    "dependencies",
                    "rusteda-core.workspace = true\nrusteda-workspace.workspace = true\nrusteda-formats.workspace = true\nrustsys.workspace = true\nrustsys-check.workspace = true\ncrossterm.workspace = true",
                ),
                (
                    "rustsys-cli",
                    "dev-dependencies",
                    "rustsys-fixtures.workspace = true\nrustsys-defects.workspace = true",
                ),
                ("xtask", "dependencies", "rustsys-fixtures.workspace = true"),
            ],
        );
        t.file(
            "Cargo.toml",
            "[workspace.dependencies]\ncudarc = { version = \"=0.19.10\" }\n",
        );
        assert_eq!(errors(&t), Vec::<String>::new());
    }

    // AGENTS 6.2, 7.1, 9.2 and the RustEDA contract: forbidden edges are reported with their
    // manifest line and section.
    #[test]
    fn forbidden_edges() {
        let cases: [(&str, &str, &str, &str); 29] = [
            (
                "rustsys",
                "dependencies",
                "rustsys-demo.workspace = true",
                "another engine",
            ),
            (
                "rustsys-demo",
                "dev-dependencies",
                "rustsys.workspace = true",
                "another engine",
            ),
            (
                "rustsys",
                "dependencies",
                "rustsys-view.workspace = true",
                "crate does not depend on the gui group",
            ),
            (
                "rustsys",
                "dependencies",
                "crossterm = \"0.29\"",
                "user-interface",
            ),
            ("rustsys", "dependencies", "egui = \"1\"", "user-interface"),
            // A companion crate of a toolkit counts by its name prefix.
            (
                "rusteda-core",
                "dependencies",
                "crossterm_winapi = \"0.9\"",
                "user-interface",
            ),
            (
                "rusteda-formats",
                "dependencies",
                "epaint = \"1\"",
                "user-interface",
            ),
            (
                "rustsys-check",
                "dependencies",
                "rustsys-view.workspace = true",
                "a verify crate does not depend on the gui group",
            ),
            (
                "rustsys-check",
                "dependencies",
                "rustsys.workspace = true",
                "never depend on an engine",
            ),
            (
                "rustsys-check",
                "dev-dependencies",
                "rustsys.workspace = true",
                "never depend on an engine",
            ),
            (
                "rustsys-defects",
                "dev-dependencies",
                "rustsys.workspace = true",
                "never depend on an engine",
            ),
            (
                "rustsys-fixtures",
                "dependencies",
                "rustsys-check.workspace = true",
                "verify group",
            ),
            (
                "rustsys",
                "dependencies",
                "rustsys-cli.workspace = true",
                "apps crate",
            ),
            (
                "rustsys-view",
                "dev-dependencies",
                "rusteda-cli.workspace = true",
                "apps crate",
            ),
            (
                "rustsys-cli",
                "dependencies",
                "rusteda-cli.workspace = true",
                "apps crate",
            ),
            // The shared crates: none uses a crate of the tool, and of the shared crates only those
            // below it.
            (
                "rusteda-formats",
                "dependencies",
                "rustsys.workspace = true",
                "rusteda-formats uses only rusteda-core",
            ),
            (
                "rusteda-formats",
                "dev-dependencies",
                "rustsys-fixtures.workspace = true",
                "rusteda-formats uses only rusteda-core",
            ),
            (
                "rusteda-formats",
                "dependencies",
                "rusteda-compute.workspace = true",
                "rusteda-formats uses only rusteda-core",
            ),
            (
                "rusteda-compute",
                "dependencies",
                "rusteda-workspace.workspace = true",
                "rusteda-compute uses only rusteda-core",
            ),
            (
                "rusteda-workspace",
                "build-dependencies",
                "rusteda-formats.workspace = true",
                "rusteda-workspace uses only rusteda-core",
            ),
            (
                "rusteda-core",
                "dependencies",
                "rusteda-compute.workspace = true",
                "rusteda-core uses no other workspace crate",
            ),
            (
                "rusteda-cli",
                "dependencies",
                "rustsys-check.workspace = true",
                "rusteda-cli uses only rusteda-core, rusteda-workspace",
            ),
            // GPU bindings belong to rusteda-compute, also as companion crates and in the
            // manifests outside the crate groups.
            (
                "rustsys",
                "dependencies",
                "cudarc = { workspace = true, optional = true }",
                "GPU bindings",
            ),
            (
                "rustsys-cli",
                "dependencies",
                "wgpu-core = \"25\"",
                "GPU bindings",
            ),
            (
                "rusteda-core",
                "dev-dependencies",
                "objc2-metal = \"0.3\"",
                "GPU bindings",
            ),
            (
                "rustsys",
                "dependencies",
                "cubecl = { version = \"0.6\" }",
                "GPU bindings",
            ),
            (
                "rustsys-check",
                "dependencies",
                "metal = \"0.32\"",
                "GPU bindings",
            ),
            (
                "rusteda-workspace",
                "dependencies",
                "cudarc_sys = \"1\"",
                "GPU bindings",
            ),
            ("xtask", "dependencies", "cudarc = \"0.19\"", "GPU bindings"),
        ];
        for (who, section, line, why) in cases {
            let t = tree("deps-bad", &[(who, section, line)]);
            let e = errors(&t);
            assert_eq!(e.len(), 1, "{who} [{section}] {line}: {e:?}");
            assert!(e[0].contains(why), "{who} {line}: {}", e[0]);
            assert!(e[0].contains(&format!("[{section}]")), "{}", e[0]);
            assert!(
                e[0].contains(&format!("{who} must not depend on")),
                "{}",
                e[0]
            );
        }
    }

    // AGENTS 7.1: a renamed dependency is judged by its package, also when the rename is made in
    // a value over several lines or in the root manifest's workspace dependencies.
    #[test]
    fn renames_do_not_hide_edges() {
        for (root, line) in [
            ("", "engine = { package = \"rustsys\", path = \"x\" }"),
            (
                "",
                "engine = { path = \"x\", features = [\n    \"a\",\n], package = \"rustsys\" }",
            ),
            (
                "[workspace.dependencies]\nengine = { path = \"x\", package = \"rustsys\" }\n",
                "engine.workspace = true",
            ),
            (
                "[workspace.dependencies]\ngpu = { version = \"=0.19.10\", package = \"cudarc\" }\n",
                "gpu.workspace = true",
            ),
        ] {
            let t = tree("deps-rename", &[("rustsys-check", "dependencies", line)]);
            if !root.is_empty() {
                t.file("Cargo.toml", root);
            }
            let e = errors(&t);
            assert_eq!(e.len(), 1, "{line}: {e:?}");
            assert!(
                e[0].contains("must not depend on rustsys")
                    || e[0].contains("must not depend on cudarc"),
                "{}",
                e[0]
            );
        }
    }

    // AGENTS 7.1: checker crates must carry `#![forbid(unsafe_code)]`.
    #[test]
    fn checker_forbids_unsafe_code() {
        let ok = tree("deps-check-ok", &[]);
        ok.file(
            "crates/verify/rustsys-check/src/lib.rs",
            "#![forbid(unsafe_code)]\n",
        );
        assert!(errors(&ok).is_empty());

        let bad = tree("deps-check-bad", &[]);
        bad.file(
            "crates/verify/rustsys-check/src/lib.rs",
            "// missing forbid\n",
        );
        let e = errors(&bad);
        assert_eq!(e.len(), 1, "{e:?}");
        assert!(e[0].contains("#![forbid(unsafe_code)]"), "{}", e[0]);
    }
}
