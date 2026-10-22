// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Reading the netlist sources (CHECK-REQ-0020): preprocessed and parsed as the RTL is, with the
//! tokens scanned for a drive strength, which the parser skips (CHECK-REQ-0023).

use rusteda_formats::verilog::ast::SourceUnit;
use rusteda_formats::verilog::token::{Tok, Token};
use rusteda_formats::verilog::{
    Diagnostic, PpOptions, Preprocessor, SourceMap, Span, parse_tokens,
};

use super::{THREE_STATE, unmodeled};
use crate::Input;

/// Reads the netlist sources in order with one preprocessor, as the RTL is read, and rejects a
/// continuous assignment whose drive strength floats its target (CHECK-REQ-0023).
pub(crate) fn read(sm: &mut SourceMap, input: &Input) -> Result<Vec<SourceUnit>, String> {
    let count = match input {
        Input::Files(f) => f.len(),
        Input::Texts(t) => t.len(),
    };
    let mut pp = Preprocessor::new(PpOptions::default());
    let mut units = Vec::with_capacity(count);
    for k in 0..count {
        let id = match input {
            Input::Files(f) => sm.load(&f[k]).map_err(|e| {
                Diagnostic::error(
                    "V0002",
                    Span::default(),
                    format!("cannot read '{}': {e}", f[k].display()),
                )
                .render(sm)
            })?,
            Input::Texts(t) => sm.add(t[k].0.as_str(), t[k].1.as_str()),
        };
        let toks = pp.run(sm, id);
        let errors: Vec<String> = pp
            .diags
            .drain(..)
            .filter(Diagnostic::is_error)
            .map(|d| d.render(sm))
            .collect();
        if !errors.is_empty() {
            return Err(errors.join("\n"));
        }
        strengths(sm, &toks)?;
        units.push(parse_tokens(&toks).map_err(|d| d.render(sm))?);
    }
    Ok(units)
}

/// Rejects a continuous assignment with a `highz0` or `highz1` drive strength: it leaves its
/// target at high impedance for one value. Other strengths do not change the value of a net with
/// one driver, the only nets the model has.
fn strengths(sm: &SourceMap, toks: &[Token]) -> Result<(), String> {
    for (k, t) in toks.iter().enumerate() {
        // `assign (` opens a drive strength: a continuous assignment's target never starts with
        // a parenthesis.
        if t.tok != Tok::Kw("assign") || toks.get(k + 1).is_none_or(|p| p.tok != Tok::Op("(")) {
            continue;
        }
        let Some(close) = toks[k..]
            .iter()
            .position(|x| x.tok == Tok::Op(")"))
            .map(|p| k + p)
        else {
            continue;
        };
        let Some(h) = toks[k..close].iter().find_map(|x| match x.tok {
            Tok::Kw(s @ ("highz0" | "highz1")) => Some(s),
            _ => None,
        }) else {
            continue;
        };
        // The assignment up to its `=`: as written when its tokens are in order in one file,
        // token by token when a macro supplied some of them.
        let end = toks[close..]
            .iter()
            .position(|x| x.tok == Tok::Op("="))
            .map_or(close, |p| close + p - 1);
        let part = &toks[k..=end];
        let written = part
            .windows(2)
            .all(|w| w[0].span.file == w[1].span.file && w[0].span.end <= w[1].span.start);
        let text = if written {
            sm.snippet(t.span.to(toks[end].span)).to_string()
        } else {
            part.iter()
                .map(|x| match &x.tok {
                    Tok::Kw(w) | Tok::Op(w) => (*w).to_string(),
                    Tok::Ident(w) | Tok::Dec(w) => w.clone(),
                    Tok::EscIdent(w) => format!("\\{w} "),
                    _ => sm.snippet(x.span).to_string(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        return Err(unmodeled(
            THREE_STATE,
            sm,
            t.span,
            format!(
                "continuous assignment `{text}`: the drive strength {h} leaves its target at \
                 high impedance for one value; the checker does not model high impedance"
            ),
        ));
    }
    Ok(())
}
