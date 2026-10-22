// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The models of BLIF files before flattening (BLIF-REQ-0020 to BLIF-REQ-0052): names are
//! interned per model, covers are shared.

use std::collections::HashMap;
use std::sync::Arc;

use super::lex::{Lines, Word};
use super::{
    COVER, Cover, Diag, EXDC, IGNORED, Init, LATCH, LatchType, Loc, SYNTAX, TWICE, UNSUPPORTED,
};

/// A port flag of a local signal: input (`.inputs` or `.clock`).
pub(super) const INPUT: u8 = 1;
/// A port flag of a local signal: output.
pub(super) const OUTPUT: u8 = 2;

/// A command of a model, over local signals.
#[derive(Debug, Clone)]
pub(super) enum Item {
    Names {
        inputs: Vec<u32>,
        output: u32,
        cover: Arc<Cover>,
        at: Loc,
    },
    Latch {
        input: u32,
        output: u32,
        kind: Option<LatchType>,
        control: Option<u32>,
        init: Init,
        at: Loc,
    },
    /// The `ordinal`-th `.subckt` of its model (BLIF-REQ-0041).
    Subckt {
        model: String,
        ordinal: usize,
        conns: Vec<(String, u32, Loc)>,
        at: Loc,
    },
    Cell {
        cell: String,
        conns: Vec<(String, u32)>,
        mlatch: Option<(Option<u32>, Init)>,
        at: Loc,
    },
}

/// One model of a file.
#[derive(Debug, Clone, Default)]
pub(super) struct Model {
    pub name: String,
    pub at: Loc,
    /// Local signal names and their index.
    pub names: Vec<String>,
    pub index: HashMap<String, u32>,
    /// Port flags of each local signal ([`INPUT`], [`OUTPUT`]).
    pub flags: Vec<u8>,
    pub inputs: Vec<u32>,
    pub clocks: Vec<u32>,
    pub outputs: Vec<(u32, Loc)>,
    pub items: Vec<Item>,
    pub blackbox: Option<Loc>,
    subckts: usize,
}

impl Model {
    /// The local signal of `w`, interned; a name with a control character is an error
    /// (BLIF-REQ-0011).
    fn signal(&mut self, w: &Word<'_>, at: Loc, files: &[String]) -> Result<u32, Diag> {
        if let Some(&s) = self.index.get(w.text) {
            return Ok(s);
        }
        check_name(w.text, at, files)?;
        let s = self.names.len() as u32;
        self.names.push(w.text.to_string());
        self.index.insert(w.text.to_string(), s);
        self.flags.push(0);
        Ok(s)
    }
}

/// Rejects a name with a control character (BLIF-REQ-0011).
fn check_name(name: &str, at: Loc, files: &[String]) -> Result<(), Diag> {
    if name.chars().any(|c| c < ' ' || c == '\u{7f}') {
        return Err(super::err(
            files,
            at,
            SYNTAX,
            format!("name {name:?} contains a control character"),
        ));
    }
    Ok(())
}

/// The models of all files, by name.
#[derive(Debug, Clone, Default)]
pub(super) struct Models {
    pub list: Vec<Model>,
    pub by_name: HashMap<String, usize>,
}

/// Commands without effect on the function (BLIF-REQ-0051).
const IGNORABLE: [&str; 20] = [
    ".area",
    ".delay",
    ".wire_load_slope",
    ".wire",
    ".input_arrival",
    ".default_input_arrival",
    ".output_required",
    ".default_output_required",
    ".input_drive",
    ".default_input_drive",
    ".output_load",
    ".default_output_load",
    ".max_input_load",
    ".default_max_input_load",
    ".cycle",
    ".clock_event",
    ".attr",
    ".attrib",
    ".param",
    ".cname",
];

/// Commands that change the function and are not supported (BLIF-REQ-0050).
const UNSUPPORTED_COMMANDS: [&str; 6] = [
    ".search",
    ".start_kiss",
    ".end_kiss",
    ".latch_order",
    ".code",
    ".conn",
];

/// A `.names` whose rows are being read.
struct Pending {
    inputs: Vec<u32>,
    output: u32,
    rows: Vec<u8>,
    count: usize,
    onset: Option<bool>,
    at: Loc,
}

/// The state of one file's parse.
struct Parser<'a> {
    file: u32,
    files: &'a [String],
    stem: &'a str,
    model: Option<Model>,
    names: Option<Pending>,
    /// Ignored commands: name, first location and count (BLIF-REQ-0051).
    ignored: Vec<(String, Loc, usize)>,
    in_exdc: bool,
}

/// Parses one file into `models`; warnings and errors are appended (BLIF-REQ-0020).
pub(super) fn parse_file(
    models: &mut Models,
    file: u32,
    files: &[String],
    stem: &str,
    text: &str,
    warnings: &mut Vec<Diag>,
    errors: &mut Vec<Diag>,
) {
    let mut p = Parser {
        file,
        files,
        stem,
        model: None,
        names: None,
        ignored: Vec::new(),
        in_exdc: false,
    };
    let mut lines = Lines::new(text);
    let mut any_model = false;
    let mut failed = false;
    while let Some(words) = lines.next_line() {
        let first = words[0];
        let at = p.loc(&first);
        if p.in_exdc && first.text != ".end" && first.text != ".model" {
            continue;
        }
        let r = if first.text.starts_with('.') {
            p.finish_names();
            if first.text == ".model" {
                any_model = true;
            }
            p.command(models, &words, at, warnings)
        } else {
            p.row(&words, at)
        };
        if let Err(d) = r {
            errors.push(d);
            failed = true;
            break;
        }
    }
    if failed {
        return;
    }
    p.finish_names();
    if let Err(d) = p.finish_model(models) {
        errors.push(d);
        return;
    }
    if !any_model {
        let name = files.get(file as usize).cloned().unwrap_or_default();
        errors
            .push(Diag::error(SYNTAX, "the file has no model (.model)".to_string()).at(name, 1, 1));
        return;
    }
    for (cmd, at, n) in std::mem::take(&mut p.ignored) {
        let file = files.get(at.file as usize).cloned().unwrap_or_default();
        warnings.push(
            Diag::warning(
                IGNORED,
                format!(
                    "{cmd} ({n} {}) has no effect on the logic function and is skipped",
                    if n == 1 { "occurrence" } else { "occurrences" }
                ),
            )
            .at(file, at.line, at.col),
        );
    }
}

impl Parser<'_> {
    fn loc(&self, w: &Word<'_>) -> Loc {
        Loc {
            file: self.file,
            line: w.line,
            col: w.col,
        }
    }

    fn err(&self, at: Loc, code: &str, msg: impl Into<String>) -> Diag {
        super::err(self.files, at, code, msg)
    }

    /// The current model, or an error for a command outside a model (BLIF-REQ-0020).
    fn model(&mut self, w: &Word<'_>, at: Loc) -> Result<&mut Model, Diag> {
        let files = self.files;
        let cmd = w.text;
        self.model
            .as_mut()
            .ok_or_else(|| super::err(files, at, SYNTAX, format!("{cmd} outside a model (.model)")))
    }

    /// Ends the current model and adds it (BLIF-REQ-0020).
    fn finish_model(&mut self, models: &mut Models) -> Result<(), Diag> {
        let Some(m) = self.model.take() else {
            return Ok(());
        };
        if models.by_name.contains_key(&m.name) {
            return Err(self.err(m.at, TWICE, format!("model {} is defined twice", m.name)));
        }
        models.by_name.insert(m.name.clone(), models.list.len());
        models.list.push(m);
        Ok(())
    }

    /// Ends the `.names` being read (BLIF-REQ-0030).
    fn finish_names(&mut self) {
        let Some(n) = self.names.take() else {
            return;
        };
        let cover = Cover {
            inputs: n.inputs.len(),
            count: n.count,
            rows: n.rows,
            onset: n.onset.unwrap_or(true),
        };
        if let Some(m) = self.model.as_mut() {
            m.items.push(Item::Names {
                inputs: n.inputs,
                output: n.output,
                cover: Arc::new(cover),
                at: n.at,
            });
        }
    }

    /// A row of the cover being read (BLIF-REQ-0030).
    fn row(&mut self, words: &[Word<'_>], at: Loc) -> Result<(), Diag> {
        let files = self.files;
        let Some(n) = self.names.as_mut() else {
            return Err(super::err(
                files,
                at,
                COVER,
                format!("cover row '{}' without a .names before it", words[0].text),
            ));
        };
        let (plane, out) = match (n.inputs.len(), words) {
            (0, [o]) => ("", *o),
            (k, [p, o]) if k > 0 => (p.text, *o),
            (k, _) => {
                return Err(super::err(
                    files,
                    at,
                    COVER,
                    format!(
                        "a cover row of {k} inputs is an input plane of {k} characters and an \
                         output character"
                    ),
                ));
            }
        };
        if plane.len() != n.inputs.len() || !plane.bytes().all(|c| matches!(c, b'0' | b'1' | b'-'))
        {
            return Err(super::err(
                files,
                at,
                COVER,
                format!(
                    "input plane '{plane}' is not {} characters of 0, 1 and -",
                    n.inputs.len()
                ),
            ));
        }
        let value = match out.text {
            "1" => true,
            "0" => false,
            t => {
                return Err(super::err(
                    files,
                    Loc {
                        file: at.file,
                        line: out.line,
                        col: out.col,
                    },
                    COVER,
                    format!("output '{t}' of a cover row is neither 0 nor 1"),
                ));
            }
        };
        match n.onset {
            Some(v) if v != value => {
                return Err(super::err(
                    files,
                    at,
                    COVER,
                    "the rows of one cover mix the outputs 0 and 1",
                ));
            }
            _ => n.onset = Some(value),
        }
        n.rows.extend_from_slice(plane.as_bytes());
        n.count += 1;
        Ok(())
    }

    /// One command (BLIF-REQ-0020 to BLIF-REQ-0052).
    fn command(
        &mut self,
        models: &mut Models,
        words: &[Word<'_>],
        at: Loc,
        warnings: &mut Vec<Diag>,
    ) -> Result<(), Diag> {
        let cmd = words[0].text;
        let args = &words[1..];
        match cmd {
            ".model" => {
                self.finish_model(models)?;
                self.in_exdc = false;
                if args.len() > 1 {
                    return Err(self.err(self.loc(&args[1]), SYNTAX, ".model takes one name"));
                }
                let name = args
                    .first()
                    .map_or_else(|| self.stem.to_string(), |w| w.text.into());
                check_name(&name, at, self.files)?;
                self.model = Some(Model {
                    name,
                    at,
                    ..Model::default()
                });
                Ok(())
            }
            ".end" => {
                self.in_exdc = false;
                self.model(&words[0], at)?;
                self.finish_model(models)
            }
            ".inputs" | ".outputs" | ".clock" => self.ports(cmd, args, at),
            ".names" => {
                if args.is_empty() {
                    return Err(self.err(at, SYNTAX, ".names needs an output signal"));
                }
                let files = self.files;
                let file = self.file;
                let m = self.model(&words[0], at)?;
                let mut sig = Vec::with_capacity(args.len());
                for w in args {
                    let a = Loc {
                        file,
                        line: w.line,
                        col: w.col,
                    };
                    sig.push(m.signal(w, a, files)?);
                }
                let output = sig.pop().unwrap_or_default();
                self.names = Some(Pending {
                    inputs: sig,
                    output,
                    rows: Vec::new(),
                    count: 0,
                    onset: None,
                    at,
                });
                Ok(())
            }
            ".latch" => self.latch(&words[0], args, at),
            ".subckt" => self.subckt(&words[0], args, at),
            ".gate" | ".mlatch" => self.cell(&words[0], args, at),
            ".exdc" => {
                self.model(&words[0], at)?;
                self.in_exdc = true;
                let file = self
                    .files
                    .get(at.file as usize)
                    .cloned()
                    .unwrap_or_default();
                warnings.push(
                    Diag::warning(
                        EXDC,
                        "the external don't-care network (.exdc) is skipped: the function of the \
                         model is kept exactly",
                    )
                    .at(file, at.line, at.col),
                );
                Ok(())
            }
            ".blackbox" => {
                let m = self.model(&words[0], at)?;
                m.blackbox = Some(at);
                Ok(())
            }
            c if UNSUPPORTED_COMMANDS.contains(&c) => Err(self.err(
                at,
                UNSUPPORTED,
                format!("{c} is not supported: it would change the function if ignored"),
            )),
            c if IGNORABLE.contains(&c) => {
                self.model(&words[0], at)?;
                match self.ignored.iter_mut().find(|(n, _, _)| n == c) {
                    Some((_, _, n)) => *n += 1,
                    None => self.ignored.push((c.to_string(), at, 1)),
                }
                Ok(())
            }
            c => Err(self.err(
                at,
                UNSUPPORTED,
                format!("unknown command {c}: it may change the function"),
            )),
        }
    }

    /// `.inputs`, `.outputs` and `.clock` (BLIF-REQ-0021).
    fn ports(&mut self, cmd: &str, args: &[Word<'_>], at: Loc) -> Result<(), Diag> {
        let files = self.files;
        let file = self.file;
        let first = Word {
            text: cmd,
            line: at.line,
            col: at.col,
        };
        let m = self.model(&first, at)?;
        for w in args {
            let a = Loc {
                file,
                line: w.line,
                col: w.col,
            };
            let s = m.signal(w, a, files)?;
            let flag = if cmd == ".outputs" { OUTPUT } else { INPUT };
            if m.flags[s as usize] & flag != 0 {
                return Err(super::err(
                    files,
                    a,
                    TWICE,
                    format!(
                        "{} is declared twice as {}",
                        w.text,
                        if flag == OUTPUT {
                            "an output"
                        } else {
                            "an input"
                        }
                    ),
                ));
            }
            m.flags[s as usize] |= flag;
            match cmd {
                ".inputs" => m.inputs.push(s),
                ".clock" => m.clocks.push(s),
                _ => m.outputs.push((s, a)),
            }
        }
        Ok(())
    }

    /// `.latch in out [type control] [init]` (BLIF-REQ-0031).
    fn latch(&mut self, w0: &Word<'_>, args: &[Word<'_>], at: Loc) -> Result<(), Diag> {
        let files = self.files;
        let file = self.file;
        let la = |w: &Word<'_>| Loc {
            file,
            line: w.line,
            col: w.col,
        };
        let init = |w: &Word<'_>| match w.text {
            "0" => Ok(Init::Zero),
            "1" => Ok(Init::One),
            "2" => Ok(Init::DontCare),
            "3" => Ok(Init::Unknown),
            t => Err(super::err(
                files,
                la(w),
                LATCH,
                format!("latch initial value '{t}' is not 0, 1, 2 or 3"),
            )),
        };
        let kind = |w: &Word<'_>| match w.text {
            "fe" => Ok(LatchType::FallingEdge),
            "re" => Ok(LatchType::RisingEdge),
            "ah" => Ok(LatchType::ActiveHigh),
            "al" => Ok(LatchType::ActiveLow),
            "as" => Err(super::err(
                files,
                la(w),
                UNSUPPORTED,
                "latch type 'as' (asynchronous) is not supported: BLIF does not define its \
                 behavior",
            )),
            t => Err(super::err(
                files,
                la(w),
                LATCH,
                format!("latch type '{t}' is not fe, re, ah or al"),
            )),
        };
        let (k, ctl, i) = match args {
            [_, _] => (None, None, Init::Unknown),
            [_, _, v] => (None, None, init(v)?),
            [_, _, t, c] => (Some(kind(t)?), Some(*c), Init::Unknown),
            [_, _, t, c, v] => (Some(kind(t)?), Some(*c), init(v)?),
            _ => {
                return Err(super::err(
                    files,
                    at,
                    LATCH,
                    ".latch takes an input, an output, optionally a type and a control, and \
                     optionally an initial value",
                ));
            }
        };
        let m = self.model(w0, at)?;
        let input = m.signal(&args[0], la(&args[0]), files)?;
        let output = m.signal(&args[1], la(&args[1]), files)?;
        let control = match ctl {
            Some(c) if c.text != "NIL" => Some(m.signal(&c, la(&c), files)?),
            _ => None,
        };
        m.items.push(Item::Latch {
            input,
            output,
            kind: k,
            control,
            init: i,
            at,
        });
        Ok(())
    }

    /// `formal=actual` connections (BLIF-REQ-0033, BLIF-REQ-0040).
    fn conns(
        &mut self,
        w0: &Word<'_>,
        words: &[Word<'_>],
        at: Loc,
    ) -> Result<Vec<(String, u32, Loc)>, Diag> {
        let files = self.files;
        let file = self.file;
        let m = self.model(w0, at)?;
        let mut out: Vec<(String, u32, Loc)> = Vec::with_capacity(words.len());
        for w in words {
            let a = Loc {
                file,
                line: w.line,
                col: w.col,
            };
            let Some((f, act)) = w.text.split_once('=') else {
                return Err(super::err(
                    files,
                    a,
                    SYNTAX,
                    format!("connection '{}' is not formal=actual", w.text),
                ));
            };
            if f.is_empty() || act.is_empty() {
                return Err(super::err(
                    files,
                    a,
                    SYNTAX,
                    format!("connection '{}' is not formal=actual", w.text),
                ));
            }
            if out.iter().any(|(x, _, _)| x == f) {
                return Err(super::err(
                    files,
                    a,
                    SYNTAX,
                    format!("formal {f} is connected twice"),
                ));
            }
            let actual = Word {
                text: act,
                line: w.line,
                col: w.col + f.chars().count() as u32 + 1,
            };
            let s = m.signal(&actual, a, files)?;
            out.push((f.to_string(), s, a));
        }
        Ok(out)
    }

    /// `.subckt model formal=actual...` (BLIF-REQ-0040).
    fn subckt(&mut self, w0: &Word<'_>, args: &[Word<'_>], at: Loc) -> Result<(), Diag> {
        let Some((model, rest)) = args.split_first() else {
            return Err(self.err(at, SYNTAX, ".subckt needs a model name"));
        };
        let conns = self.conns(w0, rest, at)?;
        let m = self.model(w0, at)?;
        let ordinal = m.subckts;
        m.subckts += 1;
        m.items.push(Item::Subckt {
            model: model.text.to_string(),
            ordinal,
            conns,
            at,
        });
        Ok(())
    }

    /// `.gate cell formal=actual...` and `.mlatch cell formal=actual... control [init]`
    /// (BLIF-REQ-0033).
    fn cell(&mut self, w0: &Word<'_>, args: &[Word<'_>], at: Loc) -> Result<(), Diag> {
        let Some((cell, rest)) = args.split_first() else {
            return Err(self.err(at, SYNTAX, format!("{} needs a cell name", w0.text)));
        };
        let mlatch = w0.text == ".mlatch";
        let n = rest.iter().take_while(|w| w.text.contains('=')).count();
        let (conns, tail) = rest.split_at(n);
        let conns = self.conns(w0, conns, at)?;
        let files = self.files;
        let file = self.file;
        let m = self.model(w0, at)?;
        let ml = match (mlatch, tail) {
            (false, []) => None,
            (true, [c]) | (true, [c, _]) => {
                let a = Loc {
                    file,
                    line: c.line,
                    col: c.col,
                };
                let control = if c.text == "NIL" {
                    None
                } else {
                    Some(m.signal(c, a, files)?)
                };
                let init = match tail.get(1).map(|w| w.text) {
                    None | Some("3") => Init::Unknown,
                    Some("0") => Init::Zero,
                    Some("1") => Init::One,
                    Some("2") => Init::DontCare,
                    Some(t) => {
                        return Err(super::err(
                            files,
                            a,
                            LATCH,
                            format!("latch initial value '{t}' is not 0, 1, 2 or 3"),
                        ));
                    }
                };
                Some((control, init))
            }
            _ => {
                return Err(super::err(
                    files,
                    at,
                    SYNTAX,
                    if mlatch {
                        ".mlatch takes a cell, formal=actual connections, a control and \
                         optionally an initial value"
                    } else {
                        ".gate takes a cell and formal=actual connections"
                    },
                ));
            }
        };
        m.items.push(Item::Cell {
            cell: cell.text.to_string(),
            conns: conns.into_iter().map(|(f, s, _)| (f, s)).collect(),
            mlatch: ml,
            at,
        });
        Ok(())
    }
}
