// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The crates of the repository (AGENTS 6.1): the shared crates of the RustEDA tools
//! ([`crate::shared::CRATES`]) and the crates of this tool ([`TOOL`]), each in its group and in a
//! directory named after its package.

use super::Report;
use super::manifest;
use std::path::Path;

/// The crates of this tool: group and package name.
pub(super) const TOOL: [(&str, &str); 5] = [
    ("engines", "rustsys"),
    ("verify", "rustsys-check"),
    ("verify", "rustsys-fixtures"),
    ("verify", "rustsys-defects"),
    ("apps", "rustsys-cli"),
];

/// Checks the package of every crate manifest `crates/<group>/<dir>/Cargo.toml` in `files`.
pub(super) fn check(root: &Path, files: &[String], r: &mut Report) {
    let known = || crate::shared::CRATES.iter().chain(TOOL.iter());
    for f in files {
        let parts: Vec<&str> = f.split('/').collect();
        let ["crates", group, dir, "Cargo.toml"] = parts.as_slice() else {
            continue;
        };
        let text = std::fs::read_to_string(root.join(f)).unwrap_or_default();
        let Some(name) = manifest::parse(&text).name else {
            r.errors.push(format!("{f}: no package name"));
            continue;
        };
        match known().find(|(_, n)| *n == name) {
            None => {
                let names: Vec<&str> = known().map(|(_, n)| *n).collect();
                r.errors.push(format!(
                    "{f}: {name} is neither a shared RustEDA crate nor a crate of this tool \
                     (crates: {})",
                    names.join(", ")
                ));
            }
            Some((g, _)) if g != group => {
                r.errors
                    .push(format!("{f}: {name} belongs in crates/{g}/{name}/"));
            }
            Some(_) if name != *dir => {
                r.errors
                    .push(format!("{f}: {name} belongs in crates/{group}/{name}/"));
            }
            Some(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Tree, manifest};

    fn errors(t: &Tree) -> Vec<String> {
        let mut r = Report::default();
        check(t.root(), &t.files(), &mut r);
        r.errors
    }

    // AGENTS 6.1: the shared crates, also those that come later, and the crates of this tool pass
    // in their groups.
    #[test]
    fn known_crates_pass() {
        let t = Tree::new("packages-ok");
        for (group, name) in crate::shared::CRATES.iter().chain(TOOL.iter()) {
            t.file(
                &format!("crates/{group}/{name}/Cargo.toml"),
                &manifest(name, ""),
            );
        }
        t.file("crates/core/rusteda-core/src/Cargo.toml", "not a manifest");
        assert_eq!(errors(&t), Vec::<String>::new());
    }

    // AGENTS 6.1: an unknown package, a crate in another group, a directory that is not named
    // after its package and a manifest without a package name are errors.
    #[test]
    fn unknown_or_misplaced_crates() {
        for (path, name, want) in [
            (
                "crates/engines/demo",
                "rustsys-demo",
                "rustsys-demo is neither a shared RustEDA crate nor a crate of this tool",
            ),
            (
                "crates/core/rusteda-extra",
                "rusteda-extra",
                "rusteda-extra is neither",
            ),
            (
                "crates/formats/rusteda-core",
                "rusteda-core",
                "rusteda-core belongs in crates/core/rusteda-core/",
            ),
            (
                "crates/apps/rustsys",
                "rustsys",
                "rustsys belongs in crates/engines/rustsys/",
            ),
            (
                "crates/apps/cli",
                "rustsys-cli",
                "rustsys-cli belongs in crates/apps/rustsys-cli/",
            ),
        ] {
            let t = Tree::new("packages-bad");
            t.file(&format!("{path}/Cargo.toml"), &manifest(name, ""));
            let e = errors(&t);
            assert_eq!(e.len(), 1, "{path}: {e:?}");
            assert!(
                e[0].starts_with(&format!("{path}/Cargo.toml: ")) && e[0].contains(want),
                "{}",
                e[0]
            );
        }
        let t = Tree::new("packages-unnamed");
        t.file("crates/verify/rustsys-check/Cargo.toml", "[lib]\n");
        assert_eq!(
            errors(&t),
            ["crates/verify/rustsys-check/Cargo.toml: no package name"]
        );
    }
}
