// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The fixture index `verification/fixtures/synthetic/synth/rtl/index.toml`: the designs that
//! `verify pdk` synthesizes, read with the TOML subset of [`super::manifest`].

use std::path::Path;

use super::manifest::{self, Fields};

/// The fixture designs, relative to the repository root.
pub(crate) const RTL: &str = "verification/fixtures/synthetic/synth/rtl";

/// One design of the fixture index: its top module and its sources (relative to its directory
/// `<name>/` beside the index), in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Design {
    pub(crate) name: String,
    pub(crate) top: String,
    pub(crate) files: Vec<String>,
}

/// The designs of the fixture index text, in file order; an index without designs is an error,
/// so that the suite never passes on zero runs.
pub(crate) fn designs(path: &str, text: &str) -> Result<Vec<Design>, String> {
    let mut out: Vec<Design> = Vec::new();
    for t in manifest::parse(path, text)? {
        if t.name != "design" {
            return Err(format!(
                "{path}:{}: unknown table [[{}]] (expected [[design]])",
                t.line, t.name
            ));
        }
        let mut f = Fields::new(path, &t);
        let d = Design {
            name: f.str("name")?,
            top: f.str("top")?,
            files: f
                .opt_list("files")?
                .ok_or_else(|| f.err("missing 'files'"))?,
        };
        let plain = manifest::plain_name(&d.name)
            && !d.files.is_empty()
            && d.files.iter().all(|p| manifest::plain_path(p));
        if !plain {
            return Err(f.err("a design needs a plain name and plain relative source paths"));
        }
        f.done()?;
        if out.iter().any(|o| o.name == d.name) {
            return Err(format!("{path}:{}: design '{}' repeated", t.line, d.name));
        }
        out.push(d);
    }
    if out.is_empty() {
        return Err(format!("{path}: no [[design]]"));
    }
    Ok(out)
}

/// The designs of the fixture index of the repository at `root`.
pub(crate) fn load(root: &Path) -> Result<Vec<Design>, String> {
    let index = root.join(RTL).join("index.toml");
    let text = std::fs::read_to_string(&index).map_err(|e| format!("{}: {e}", index.display()))?;
    designs(&format!("{RTL}/index.toml"), &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    // The fixture index lists designs with their top modules and sources; an index without
    // designs fails, so the suite cannot pass on zero runs.
    #[test]
    fn reads_design_index() {
        let text = "[[design]]\nname = \"alu\"\ntop = \"alu\"\nfiles = [\"pkg.sv\", \"alu.sv\"]\n";
        assert_eq!(
            designs("i.toml", text),
            Ok(vec![Design {
                name: "alu".into(),
                top: "alu".into(),
                files: strings(&["pkg.sv", "alu.sv"]),
            }])
        );
        assert_eq!(
            designs("i.toml", "[[design]]\nname = \"a\"\ntop = \"a\"\n"),
            Err("i.toml:1: [[design]]: missing 'files'".into())
        );
        assert!(designs("i.toml", &format!("{text}{text}")).is_err());
        assert!(designs("i.toml", &text.replace("alu.sv", "../alu.sv")).is_err());
        assert!(designs("i.toml", &text.replace("[[design]]", "[[library]]")).is_err());
        for empty in ["", "# Generated; do not edit.\n"] {
            assert_eq!(
                designs("i.toml", empty),
                Err("i.toml: no [[design]]".into())
            );
        }
    }

    // Every design of the tracked fixture index has its sources.
    #[test]
    fn tracked_design_index() {
        let Some(root) = Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
            return;
        };
        if !root.join(RTL).join("index.toml").is_file() {
            return;
        }
        let designs = load(root).unwrap_or_else(|e| panic!("{e}"));
        assert!(designs.len() >= 10);
        for d in &designs {
            for f in &d.files {
                assert!(
                    root.join(RTL).join(&d.name).join(f).is_file(),
                    "{}/{f}",
                    d.name
                );
            }
        }
    }
}
