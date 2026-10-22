// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Library cells of the netlist model: the cell table, the behavior outside the model and the
//! cell functions (CHECK-REQ-0020, CHECK-REQ-0021, CHECK-REQ-0023).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use rusteda_formats::liberty::{Cell, Direction as LDir, Library, SeqKind, Sequential};
use rusteda_formats::verilog::ast;
use rusteda_formats::verilog::{SourceMap, Span};

use super::{DUAL_EDGE, LIBERTY, SeqInst, THREE_STATE, UNMODELED_CELL, located, unmodeled};
use crate::circ::{B, Circuit};
use crate::lfunc;
use crate::liberty::Unread;

/// The cells of the libraries by name. A name that two definitions describe with different
/// behavior maps to the difference, which an instance of the cell reports: the verdict must not
/// depend on the order of the libraries. Identical copies, such as the corners of one library,
/// are one cell.
pub(super) fn table(libs: &[Library]) -> HashMap<&str, Result<&Cell, String>> {
    let mut out: HashMap<&str, Result<&Cell, String>> = HashMap::new();
    let mut first_in: HashMap<&str, &str> = HashMap::new();
    for l in libs {
        for cl in &l.cells {
            match out.get(cl.name.as_str()) {
                None => {
                    out.insert(&cl.name, Ok(cl));
                    first_in.insert(&cl.name, &l.name);
                }
                Some(Ok(first)) => {
                    if let Some(d) = difference(first, cl) {
                        let msg = format!(
                            "libraries {} and {} define it differently ({d})",
                            first_in[cl.name.as_str()],
                            l.name
                        );
                        out.insert(&cl.name, Err(msg));
                    }
                }
                Some(Err(_)) => {}
            }
        }
    }
    out
}

/// Text of a Liberty expression without quotes and with white space only where it is an AND,
/// between two operands.
fn norm(t: &str) -> String {
    let op = |c: char| "&|^!'()+*".contains(c);
    let mut out = String::new();
    let mut space = false;
    for c in t.chars().filter(|c| *c != '"') {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && out.chars().last().is_some_and(|p| !op(p)) && !op(c) {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    out
}

/// The first difference in the behavior that the checker reads from two definitions of a cell.
fn difference(a: &Cell, b: &Cell) -> Option<String> {
    let text = |t: &Option<String>| t.as_deref().map(norm);
    let mut pa: Vec<&str> = a.pins.iter().map(|p| p.name.as_str()).collect();
    let mut pb: Vec<&str> = b.pins.iter().map(|p| p.name.as_str()).collect();
    pa.sort_unstable();
    pb.sort_unstable();
    if pa != pb {
        return Some("their pins differ".into());
    }
    for p in &a.pins {
        let q = b.pin(&p.name)?;
        if p.direction != q.direction {
            return Some(format!("direction of pin {}", p.name));
        }
        if text(&p.function_text) != text(&q.function_text) {
            let show =
                |t: &Option<String>| t.as_deref().map_or("none".into(), |t| format!("{t:?}"));
            return Some(format!(
                "function of pin {}: {} and {}",
                p.name,
                show(&p.function_text),
                show(&q.function_text)
            ));
        }
        if text(&p.three_state) != text(&q.three_state) {
            return Some(format!("three_state of pin {}", p.name));
        }
    }
    let pg = |c: &Cell| {
        let mut v = c.pg_pins.clone();
        v.sort_unstable();
        v
    };
    if pg(a) != pg(b) {
        return Some("their power and ground pins differ".into());
    }
    if a.has_statetable != b.has_statetable || a.has_bus != b.has_bus {
        return Some("statetable or bus pins".into());
    }
    let group = |s: &Sequential| {
        (
            s.kind,
            [
                Some(s.state.clone()),
                Some(s.state_n.clone()),
                Some(norm(&s.data_text)),
                Some(norm(&s.clock_text)),
                text(&s.clocked_on_also),
                text(&s.clear_text),
                text(&s.preset_text),
                text(&s.clear_preset_var1),
                text(&s.clear_preset_var2),
            ],
        )
    };
    if a.sequentials.len() != b.sequentials.len()
        || a.sequentials
            .iter()
            .zip(&b.sequentials)
            .any(|(x, y)| group(x) != group(y))
    {
        return Some("their ff or latch groups differ".into());
    }
    None
}

/// Rejects an instance of a cell whose Liberty description holds behavior the checker does not
/// model (CHECK-REQ-0023): what the library model keeps, then `unread`, the first construct of
/// the library text that it does not keep, then a description with errors (CHECK-REQ-0082). All
/// of this comes before any function of the cell is evaluated.
pub(super) fn modeled(
    sm: &SourceMap,
    i: &ast::Instance,
    cell: &Cell,
    unread: Option<&Unread>,
) -> Result<(), String> {
    let reject = |code: &str, what: String| {
        Err(unmodeled(
            code,
            sm,
            i.span,
            format!("instance {} of cell {}: {what}", i.name.name, cell.name),
        ))
    };
    if let Some(p) = cell
        .pins
        .iter()
        .find(|p| p.direction != LDir::Input && p.three_state.is_some())
    {
        return reject(
            THREE_STATE,
            format!(
                "pin {} is three-state (three_state \"{}\"); the checker does not model high \
                 impedance",
                p.name,
                p.three_state.as_deref().unwrap_or_default()
            ),
        );
    }
    if let Some(also) = cell
        .sequentials
        .iter()
        .find_map(|s| s.clocked_on_also.as_deref())
    {
        return reject(
            DUAL_EDGE,
            format!(
                "clocked_on_also \"{also}\" makes it a dual-edge or master-slave element; the \
                 checker models a single clock edge"
            ),
        );
    }
    if cell.has_statetable {
        return reject(
            UNMODELED_CELL,
            "a statetable describes the cell; the checker does not model state tables".into(),
        );
    }
    if cell.has_bus {
        return reject(
            UNMODELED_CELL,
            "the cell has bus or bundle pins, which the checker does not model".into(),
        );
    }
    if cell.sequentials.len() > 1 {
        return reject(
            UNMODELED_CELL,
            format!(
                "{} ff and latch groups; the checker models one state element per cell",
                cell.sequentials.len()
            ),
        );
    }
    let mut names = HashSet::new();
    if let Some(p) = cell.pins.iter().find(|p| !names.insert(p.name.as_str())) {
        return reject(
            UNMODELED_CELL,
            format!(
                "pin {} is described twice; the checker does not choose between descriptions",
                p.name
            ),
        );
    }
    if let Some(u) = unread {
        return reject(u.code, u.what.clone());
    }
    // The latch styles of the attribute (`latch_posedge` and the like) hold an enable latch that
    // a function alone does not describe; the checker does not tell the styles apart.
    if cell.is_clock_gating && cell.sequentials.is_empty() {
        return reject(
            UNMODELED_CELL,
            "clock_gating_integrated_cell makes it a clock-gating cell, but no ff or latch group \
             describes the state of its enable; the checker does not model the gating that the \
             attribute names"
                .into(),
        );
    }
    // A group that does not read is dropped from the model with a problem, so the problem, not
    // the missing state, is the reason.
    if !cell.problems.is_empty() {
        return reject(
            LIBERTY,
            format!(
                "the library describes it with errors: {}",
                cell.problems.join("; ")
            ),
        );
    }
    Ok(())
}

/// Builds `text`, the function of instance `i` of `cell` that `what` names, over the names that
/// `look` resolves: the connected input pins and the state variable of the cell's ff or latch
/// group. `state` gives that group's state and complement names and its state variable. A name
/// that is neither is behavior outside the model (CHECK-REQ-0023); a text that does not read is
/// an error of the library.
#[allow(clippy::too_many_arguments)]
pub(super) fn function(
    c: &mut Circuit,
    sm: &SourceMap,
    i: &ast::Instance,
    cell: &Cell,
    what: &str,
    text: &str,
    look: &dyn Fn(&str) -> Option<B>,
    state: Option<(&str, &str, u32)>,
) -> Result<B, String> {
    // The complement state name is resolved through the state variable.
    let not_q = match state {
        Some((_, n, q)) if !n.is_empty() && text.contains(n) => Some((n, c.not(B::V(q)))),
        _ => None,
    };
    let missing = RefCell::new(None::<String>);
    let l = |n: &str| -> Option<B> {
        if let Some((nn, b)) = not_q {
            if n == nn {
                return Some(b);
            }
        }
        let r = look(n);
        if r.is_none() {
            missing.borrow_mut().get_or_insert_with(|| n.to_string());
        }
        r
    };
    lfunc::build(c, text, &l).map_err(|e| {
        let at = format!("instance {} of cell {}", i.name.name, cell.name);
        match missing.take() {
            Some(name) => {
                let read = match cell.pin(&name).map(|p| p.direction) {
                    Some(LDir::Internal) => format!("internal pin {name}"),
                    Some(LDir::Inout) => format!("bidirectional pin {name}"),
                    Some(LDir::Output) => format!("output pin {name}"),
                    _ => format!(
                        "{name}, which is neither a pin of the cell nor the state of its ff or \
                         latch group"
                    ),
                };
                unmodeled(
                    UNMODELED_CELL,
                    sm,
                    i.span,
                    format!(
                        "{at}: the {what} reads {read}; the checker evaluates functions of \
                         input pins and of the state of one ff or latch group"
                    ),
                )
            }
            None => unmodeled(
                LIBERTY,
                sm,
                i.span,
                format!("{at}: cannot read the {what} \"{text}\": {e}"),
            ),
        }
    })
}

/// Shared cell semantics for text and validated disk-table readers. The
/// connection reader independently supplies its driver/input checks.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn evaluate(
    c: &mut Circuit,
    sm: &SourceMap,
    i: &ast::Instance,
    cell: &Cell,
    conn: &HashMap<&str, (B, Span)>,
    define: &impl Fn(&mut Circuit, B, B, &str, Span) -> Result<(), String>,
) -> Result<Option<SeqInst>, String> {
    let at = format!("instance {} of cell {}", i.name.name, cell.name);
    if let Some(p) = cell
        .pins
        .iter()
        .find(|p| p.direction == LDir::Input && !conn.contains_key(p.name.as_str()))
    {
        return Err(located(
            sm,
            i.span,
            format!("{at}: input pin {} is not connected", p.name),
        ));
    }
    let seq = cell.sequentials.first();
    let state = seq.map(|s| (s.state.as_str(), s.state_n.as_str(), c.free()));
    let look = |n: &str| -> Option<B> {
        if let Some((s, _, q)) = state {
            if n == s {
                return Some(B::V(q));
            }
        }
        cell.pin(n)
            .filter(|p| p.direction == LDir::Input)
            .and_then(|p| conn.get(p.name.as_str()).map(|x| x.0))
    };
    let f = |c: &mut Circuit, what: &str, text: &str| {
        function(c, sm, i, cell, what, text, &look, state)
    };
    let mut outs = Vec::new();
    for p in cell.pins.iter().filter(|p| p.direction == LDir::Output) {
        let Some(&(target, place)) = conn.get(p.name.as_str()) else {
            continue;
        };
        let Some(text) = &p.function_text else {
            return Err(unmodeled(
                UNMODELED_CELL,
                sm,
                i.span,
                format!(
                    "{at}: output pin {} has no function; the checker evaluates outputs from their Liberty function",
                    p.name
                ),
            ));
        };
        let v = f(c, &format!("function of pin {}", p.name), text)?;
        outs.push((target, v, &p.name, place));
    }
    for (t, v, pin, place) in outs {
        define(c, t, v, &format!("{at} pin {pin}"), place)?;
    }
    let Some(s) = seq else { return Ok(None) };
    let (_, _, q) = state.expect("sequential state");
    let (group, data, clock) = match s.kind {
        SeqKind::FlipFlop => ("ff", "next_state", "clocked_on"),
        SeqKind::Latch => ("latch", "data_in", "enable"),
    };
    let of = |attr: &str| format!("{attr} of its {group} group");
    let next = f(c, &of(data), &s.data_text)?;
    let clk = f(c, &of(clock), &s.clock_text)?;
    let clear = match &s.clear_text {
        Some(t) => f(c, &of("clear"), t)?,
        None => B::F,
    };
    let preset = match &s.preset_text {
        Some(t) => f(c, &of("preset"), t)?,
        None => B::F,
    };
    Ok(Some(SeqInst {
        inst: i.name.name.clone(),
        cell: cell.name.clone(),
        latch: s.kind == SeqKind::Latch,
        iq: q,
        next,
        clk,
        clear,
        preset,
    }))
}

#[cfg(test)]
mod tests {
    // CHECK-REQ-0023: two library definitions compare their function texts without quotes and
    // without the white space around operators; white space between operands is an AND.
    #[test]
    fn function_texts_keep_white_space_that_is_an_and() {
        use super::norm;
        assert_eq!(norm("\"A & ( B | C )\""), "A&(B|C)");
        assert_eq!(norm(" A'  B "), "A'B");
        assert_eq!(norm("A  B"), "A B");
        assert_ne!(norm("A B"), norm("AB"));
    }
}
