// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! SystemVerilog reader and structural netlist writer (`docs/specs/verilog/`).

pub mod ast;
pub mod diag;
pub mod lex;
pub mod netlist;
pub mod number;
mod parse;
pub mod pp;
pub mod span;
pub mod token;
pub mod write;

use std::path::PathBuf;

pub use diag::{Diagnostic, Severity};
pub use parse::parse_tokens;
pub use pp::{PpOptions, Preprocessor};
pub use span::{FileId, SourceMap, Span};

/// Reads, preprocesses and parses source files in order (VERILOG-REQ-0206). Returns the parsed
/// units and all diagnostics; parsing stops at the first file with an error.
pub fn read_sources(
    sm: &mut SourceMap,
    files: &[PathBuf],
    options: PpOptions,
) -> (Vec<ast::SourceUnit>, Vec<Diagnostic>) {
    let mut pp = Preprocessor::new(options);
    let mut units = Vec::new();
    let mut diags = Vec::new();
    for f in files {
        let id = match sm.load(f) {
            Ok(id) => id,
            Err(e) => {
                diags.push(Diagnostic::error(
                    "V0002",
                    Span::default(),
                    format!("cannot read '{}': {e}", f.display()),
                ));
                return (units, diags);
            }
        };
        if !parse_one(sm, &mut pp, id, &mut units, &mut diags) {
            break;
        }
    }
    (units, diags)
}

/// Preprocesses and parses in-memory sources, in order.
pub fn read_texts(
    sm: &mut SourceMap,
    texts: &[(&str, &str)],
    options: PpOptions,
) -> (Vec<ast::SourceUnit>, Vec<Diagnostic>) {
    let mut pp = Preprocessor::new(options);
    let mut units = Vec::new();
    let mut diags = Vec::new();
    for (name, text) in texts {
        let id = sm.add(*name, *text);
        if !parse_one(sm, &mut pp, id, &mut units, &mut diags) {
            break;
        }
    }
    (units, diags)
}

/// Parses a single expression from text, for example a command-line parameter value
/// (VERILOG-REQ-0105).
pub fn parse_expression(
    sm: &mut SourceMap,
    name: &str,
    text: &str,
) -> Result<ast::Expr, Diagnostic> {
    let id = sm.add(name, text);
    let mut pp = Preprocessor::new(PpOptions::default());
    let toks = pp.run(sm, id);
    if let Some(d) = pp.diags.into_iter().find(Diagnostic::is_error) {
        return Err(d);
    }
    parse::parse_expr_tokens(&toks)
}

fn parse_one(
    sm: &mut SourceMap,
    pp: &mut Preprocessor,
    id: FileId,
    units: &mut Vec<ast::SourceUnit>,
    diags: &mut Vec<Diagnostic>,
) -> bool {
    let toks = pp.run(sm, id);
    let had_error = pp.diags.iter().any(Diagnostic::is_error);
    diags.append(&mut pp.diags);
    if had_error {
        return false;
    }
    match parse_tokens(&toks) {
        Ok(u) => {
            units.push(u);
            true
        }
        Err(d) => {
            diags.push(d);
            false
        }
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use crate::verilog::PpOptions;
    use crate::verilog::ast::{Expr, Item, ModuleItem, SourceUnit, Stmt};
    use crate::verilog::span::SourceMap;

    pub fn parse_src(src: &str) -> SourceUnit {
        let mut sm = SourceMap::new();
        let (mut units, diags) =
            crate::verilog::read_texts(&mut sm, &[("t.sv", src)], PpOptions::default());
        assert!(
            diags.is_empty(),
            "{}",
            diags
                .iter()
                .map(|d| d.render(&sm))
                .collect::<Vec<_>>()
                .join("\n")
        );
        units.remove(0)
    }

    pub fn parse_expr(src: &str) -> Expr {
        let u = parse_src(&format!("module m; assign x = {src}; endmodule"));
        let Item::Module(m) = &u.items[0] else {
            panic!()
        };
        let ModuleItem::Assign(a) = &m.items[0] else {
            panic!("{:?}", m.items[0])
        };
        a[0].rhs.clone()
    }

    pub fn parse_stmt(src: &str) -> Stmt {
        let u = parse_src(&format!("module m; always {src} endmodule"));
        let Item::Module(m) = &u.items[0] else {
            panic!()
        };
        let ModuleItem::Always(a) = &m.items[0] else {
            panic!("{:?}", m.items[0])
        };
        a.body.clone()
    }
}

#[cfg(test)]
mod tests {
    use crate::verilog::ast::{BinOp, ExprKind};
    use crate::verilog::{SourceMap, parse_expression};

    // VERILOG-REQ-0105: one expression from a named text, preprocessed as a source of its own
    // (the macro of the second text is unknown to the last one); errors are located in the text.
    #[test]
    fn single_expression_from_text() {
        let mut sm = SourceMap::new();
        let e = parse_expression(&mut sm, "<-P W>", "4'd3 + 2").unwrap();
        assert!(
            matches!(e.kind, ExprKind::Binary(BinOp::Add, _, _)),
            "{e:?}"
        );
        let e = parse_expression(&mut sm, "<-P W>", "`define N 4\n`N * 2").unwrap();
        assert!(
            matches!(e.kind, ExprKind::Binary(BinOp::Mul, _, _)),
            "{e:?}"
        );
        for (text, want) in [
            ("", "1:1: expected an expression, found end of file"),
            ("4'd3 +", "1:6: expected an expression, found end of file"),
            ("(a", "1:2: expected ')', found end of file"),
            (
                "a b",
                "1:3: expected end of expression, found identifier 'b'",
            ),
        ] {
            let d = parse_expression(&mut sm, "<-P W>", text).unwrap_err();
            assert_eq!(
                d.render(&sm),
                format!("error[V0100]: <-P W>:{want}"),
                "{text:?}"
            );
        }
        let d = parse_expression(&mut sm, "<-P W>", "`N + 1").unwrap_err();
        assert_eq!(
            d.render(&sm),
            "error[V0200]: <-P W>:1:1: undefined macro `N"
        );
    }
}
