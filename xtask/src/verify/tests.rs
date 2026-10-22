// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Tests of the verification plan (`cargo xtask verify`).

use super::*;

fn joined(steps: &[Vec<String>]) -> Vec<String> {
    steps.iter().map(|a| a.join(" ")).collect()
}

// `verify synth` runs the suites of the command line, then the mapper's unit tests and the
// job-hook tests; `--gpu` builds the command line with every GPU runtime and adds its ignored
// tests. Other products are unknown.
#[test]
fn synth_plan() {
    let suites = CLI_SUITES.map(|t| format!("--test {t}")).join(" ");
    let p = plan("synth", Options::default()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        joined(&p),
        [
            format!("test --release -p rustsys-cli {suites}"),
            "test --release -p rustsys --lib map::".to_string(),
            "test --release -p rustsys --test job_hooks".to_string(),
        ]
    );
    let g = plan("synth", Options { gpu: true }).unwrap_or_else(|e| panic!("{e}"));
    let g = joined(&g);
    assert_eq!(
        g.first(),
        Some(&format!(
            "test --release -p rustsys-cli --features cuda {suites} -- --include-ignored"
        ))
    );
    assert_eq!(g.get(1..), joined(&p).get(1..));
    assert!(plan("check", Options::default()).is_err());
    assert!(plan("", Options::default()).is_err());
}

/// The value of `key = …` in the table `[section]` of the manifest text, if any.
fn manifest_value(text: &str, section: &str, key: &str) -> Option<String> {
    let header = format!("[{section}]");
    let mut inside = false;
    text.lines().find_map(|l| {
        let l = l.trim();
        if l.starts_with('[') {
            inside = l == header;
            return None;
        }
        let (k, v) = l.split_once('=')?;
        (inside && k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
    })
}

// On the repository itself, the plan names only packages, features and targets that exist: the
// command-line crate with its GPU feature and its integration tests, the engine's `map` module
// and its job-hook tests.
#[test]
fn synth_plan_names_existing_targets() {
    let Some(root) = Path::new(env!("CARGO_MANIFEST_DIR")).parent() else {
        return;
    };
    let cli = root.join("crates/apps/rustsys-cli");
    let engine = root.join("crates/engines/rustsys");
    let read = |p: &Path| std::fs::read_to_string(p.join("Cargo.toml")).unwrap_or_default();
    let (cli_toml, engine_toml) = (read(&cli), read(&engine));
    assert_eq!(
        manifest_value(&cli_toml, "package", "name").as_deref(),
        Some("rustsys-cli")
    );
    assert_eq!(
        manifest_value(&engine_toml, "package", "name").as_deref(),
        Some("rustsys")
    );
    assert!(
        manifest_value(&cli_toml, "features", "cuda").is_some(),
        "feature cuda"
    );
    for t in CLI_SUITES {
        let tests = cli.join("tests");
        assert!(
            tests.join(format!("{t}.rs")).is_file() || tests.join(t).join("main.rs").is_file(),
            "crates/apps/rustsys-cli/tests/{t}.rs"
        );
    }
    assert!(engine.join("tests/job_hooks.rs").is_file());
    assert!(engine.join("src/map.rs").is_file() || engine.join("src/map/mod.rs").is_file());
}
