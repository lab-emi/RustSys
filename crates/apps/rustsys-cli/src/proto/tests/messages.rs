// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Messages in events and `events.jsonl` (JOB-REQ-0007, PROTO-REQ-0053), and the engine boundary
//! of the protocol (PROTO-REQ-0002).

use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use crate::proto::exec::{emit_text, event_json};
use crate::proto::{Ctx, Diag, Event, Observer};

#[derive(Default)]
struct Collect(Mutex<Vec<Diag>>);

impl Observer for Collect {
    fn event(&self, e: &Event) {
        if let Event::Diagnostic(d) = e {
            self.0.lock().unwrap().push(d.clone());
        }
    }
}

// PROTO-REQ-0053, JOB-REQ-0007: a message event in `events.jsonl` carries the parts of the
// message beside its rendered text, and reads back to the message.
#[test]
fn events_carry_messages() {
    let d = Diag::error("V0100", "expected ';'")
        .at("top.sv", 3, 7)
        .with_help("end the statement");
    let v = event_json(&Event::Diagnostic(d.clone()), Instant::now());
    assert_eq!(v["event"], "message");
    assert_eq!(v["code"], "V0100");
    assert_eq!(v["file"], "top.sv");
    assert_eq!(v["line"], 3);
    assert_eq!(
        v["text"],
        "error[V0100]: top.sv:3:7: expected ';'\n  help: end the statement"
    );
    assert_eq!(crate::proto::message::from_json(&v), Some(d));
    let plain = event_json(
        &Event::Diagnostic(Diag::warning("S0532", "x")),
        Instant::now(),
    );
    assert!(plain.get("file").is_none() && plain.get("help").is_none());
}

// CLI-REQ-0004, JOB-REQ-0007: engine text lines that already are messages pass unchanged,
// whatever their code letter; other lines get the given code.
#[test]
fn engine_text_passes_messages() {
    let c = Collect::default();
    let text = "error[S0100]: top.sv:3:7: unknown module 'x'\n\
                warning[V0101]: cells.v:7:1: unknown directive\n\
                note[C0002]: sweep: 12 candidates: 3 proven\n\
                plain text";
    emit_text(Ctx::new(None, Some(&c)), "C0001", text);
    let got = c.0.into_inner().unwrap();
    assert_eq!(got.len(), 4);
    assert_eq!(got[0].location.as_ref().map(|l| l.line), Some(3));
    assert_eq!(got[1].code, "V0101");
    assert_eq!(got[2].location, None);
    assert_eq!(
        (got[3].code.as_str(), got[3].message.as_str()),
        ("C0001", "plain text")
    );
    for (d, line) in got.iter().zip(text.lines()) {
        if d.code != "C0001" {
            assert_eq!(d.render_first_line(), line);
        }
    }
}

/// Non-test Rust sources under a directory.
fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n == "tests") {
                    continue;
                }
                sources(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
}

// PROTO-REQ-0002: the protocol names synthesis engine items only through the engine's `view`
// module.
#[test]
fn engines_only_through_views() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    sources(&root.join("src"), &mut files);
    assert!(files.len() > 10, "{} files", files.len());
    let engine = concat!("rustsys", "::");
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        for (i, _) in text.match_indices(engine) {
            let rest = &text[i + engine.len()..];
            assert!(
                rest.starts_with("view"),
                "{}: {engine}{}",
                f.display(),
                rest.chars().take(20).collect::<String>()
            );
        }
    }
}
