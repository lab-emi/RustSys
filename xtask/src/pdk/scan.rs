// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Physical-usability hazards of a netlist that the synthesis check cannot see (the netlist is
//! equivalent, yet a physical flow cannot use it as is). They are reported, not asserted:
//!
//! - instances of cells that the Liberty library marks as isolation cells, always-on cells or
//!   level shifters (`is_isolation_cell`, `always_on`, `is_level_shifter`), attributes that
//!   cell selection does not read (SYNTH-REQ-0501);
//! - instances of cells whose name contains `sparecell`;
//! - the literal constants `1'b0` and `1'b1` (SYNTH-REQ-0522, SYNTH-REQ-0601), in `assign`
//!   statements and on cell pins.

use std::collections::BTreeMap;

/// A hazard class of a cell instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Mark {
    Isolation,
    AlwaysOn,
    LevelShifter,
    SpareCell,
}

impl Mark {
    pub(crate) const ALL: [Mark; 4] = [
        Mark::Isolation,
        Mark::AlwaysOn,
        Mark::LevelShifter,
        Mark::SpareCell,
    ];

    /// The Liberty attribute, or the name pattern, that gives the mark.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Mark::Isolation => "is_isolation_cell",
            Mark::AlwaysOn => "always_on",
            Mark::LevelShifter => "is_level_shifter",
            Mark::SpareCell => "sparecell",
        }
    }

    fn of_attribute(name: &[u8]) -> Option<Mark> {
        match name {
            b"is_isolation_cell" => Some(Mark::Isolation),
            b"always_on" => Some(Mark::AlwaysOn),
            b"is_level_shifter" => Some(Mark::LevelShifter),
            _ => None,
        }
    }
}

/// A token of Liberty text: a word, the body of a quoted string, or punctuation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tok<'a> {
    Word(&'a [u8]),
    Str(&'a [u8]),
    Punct(u8),
}

/// The next token at or after `*i`, skipping white space, `/* … */` comments and line
/// continuations.
fn token<'a>(s: &'a [u8], i: &mut usize) -> Option<Tok<'a>> {
    loop {
        let c = *s.get(*i)?;
        if c.is_ascii_whitespace() || c == b'\\' {
            *i += 1;
        } else if c == b'/' && s.get(*i + 1) == Some(&b'*') {
            let body = s.get(*i + 2..).unwrap_or_default();
            *i += 2 + body
                .windows(2)
                .position(|w| w == b"*/")
                .map_or(body.len(), |p| p + 2);
        } else {
            break;
        }
    }
    let start = *i;
    let c = *s.get(start)?;
    if matches!(c, b'(' | b')' | b'{' | b'}' | b':' | b';' | b',') {
        *i += 1;
        return Some(Tok::Punct(c));
    }
    if c == b'"' {
        let mut j = start + 1;
        while let Some(&d) = s.get(j) {
            match d {
                b'"' => break,
                b'\\' => j += 2,
                _ => j += 1,
            }
        }
        *i = (j + 1).min(s.len());
        return Some(Tok::Str(
            s.get(start + 1..j.min(s.len())).unwrap_or_default(),
        ));
    }
    let mut j = start;
    while s.get(j).is_some_and(|&d| {
        !d.is_ascii_whitespace()
            && !matches!(d, b'(' | b')' | b'{' | b'}' | b':' | b';' | b',' | b'"')
    }) {
        j += 1;
    }
    *i = j.max(start + 1);
    Some(Tok::Word(s.get(start..*i).unwrap_or_default()))
}

/// The cells of a Liberty library that set `is_isolation_cell`, `always_on` or
/// `is_level_shifter` to true in the cell group or in a group nested in it (a pin, for example),
/// with their marks in [`Mark`] order.
pub(crate) fn liberty_marks(text: &[u8]) -> BTreeMap<String, Vec<Mark>> {
    let mut out: BTreeMap<String, Vec<Mark>> = BTreeMap::new();
    // Group nesting depth; the current cell with the depth of its body.
    let mut depth = 0usize;
    let mut cell: Option<(String, usize)> = None;
    // The word that starts the statement, the attribute awaiting its value, the group header
    // being read (its name and first argument) and whether its argument list is closed.
    let mut word: Option<&[u8]> = None;
    let mut attribute: Option<&[u8]> = None;
    let mut header: Option<(&[u8], Option<&[u8]>)> = None;
    let mut closed = false;
    let mut i = 0;
    while let Some(t) = token(text, &mut i) {
        match t {
            Tok::Word(w) | Tok::Str(w) if attribute.is_some() => {
                if let (Some(a), Some((name, _))) = (attribute.take(), &cell) {
                    if let Some(m) =
                        Mark::of_attribute(a).filter(|_| w.eq_ignore_ascii_case(b"true"))
                    {
                        let marks = out.entry(name.clone()).or_default();
                        if !marks.contains(&m) {
                            marks.push(m);
                            marks.sort();
                        }
                    }
                }
            }
            Tok::Word(w) | Tok::Str(w) => {
                if let Some((_, arg)) = header.as_mut().filter(|_| !closed) {
                    arg.get_or_insert(w);
                } else if word.is_none() && header.is_none() {
                    word = Some(w);
                }
            }
            Tok::Punct(b':') => attribute = word.take(),
            Tok::Punct(b'(') => {
                if let Some(w) = word.take() {
                    header = Some((w, None));
                    closed = false;
                }
            }
            Tok::Punct(b')') => closed = true,
            Tok::Punct(b'{') => {
                depth += 1;
                if let Some((b"cell", Some(arg))) = header.filter(|_| closed) {
                    cell = Some((String::from_utf8_lossy(arg).into_owned(), depth));
                }
                (word, attribute, header) = (None, None, None);
            }
            Tok::Punct(b'}') => {
                if cell.as_ref().is_some_and(|(_, d)| *d == depth) {
                    cell = None;
                }
                depth = depth.saturating_sub(1);
                (word, attribute, header) = (None, None, None);
            }
            Tok::Punct(b';') => (word, attribute, header) = (None, None, None),
            Tok::Punct(_) => {}
        }
    }
    out
}

/// True when `s` holds the literal `1'b0` or `1'b1` as a token of its own (not inside a name).
fn literal_at(s: &[u8], at: usize) -> bool {
    let before = at.checked_sub(1).and_then(|p| s.get(p));
    let lit = s.get(at..at + 4).is_some_and(|w| {
        w.first() == Some(&b'1')
            && w.get(1) == Some(&b'\'')
            && matches!(w.get(2), Some(b'b' | b'B'))
            && matches!(w.get(3), Some(b'0' | b'1'))
    });
    let ident = |c: Option<&u8>| {
        c.is_some_and(|&c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'$' | b'\\' | b'\''))
    };
    lit && !ident(before) && !ident(s.get(at + 4))
}

/// The literal constants of a structural netlist: `assign` statements whose right-hand side is
/// `1'b0` or `1'b1`, and cell pins connected to such a constant. Escaped identifiers and
/// comments are skipped.
pub(crate) fn literal_constants(netlist: &str) -> (u64, u64) {
    let s = netlist.as_bytes();
    let (mut assigns, mut pins) = (0u64, 0u64);
    // Whether the current statement has begun and starts with `assign`, and its literals so far.
    let (mut begun, mut assign, mut literals) = (false, false, 0u64);
    let mut i = 0;
    while let Some(&c) = s.get(i) {
        let rest = s.get(i..).unwrap_or_default();
        if rest.starts_with(b"//") {
            i += rest.iter().position(|&d| d == b'\n').unwrap_or(rest.len());
            continue;
        }
        if rest.starts_with(b"/*") {
            let body = rest.get(2..).unwrap_or_default();
            i += 2 + body
                .windows(2)
                .position(|w| w == b"*/")
                .map_or(body.len(), |p| p + 2);
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if !begun {
            begun = true;
            assign = rest.starts_with(b"assign")
                && !rest
                    .get(6)
                    .is_some_and(|d| d.is_ascii_alphanumeric() || matches!(d, b'_' | b'$'));
        }
        match c {
            b'\\' => {
                // An escaped identifier runs to the next white space.
                i += rest
                    .iter()
                    .position(u8::is_ascii_whitespace)
                    .unwrap_or(rest.len());
                continue;
            }
            b';' => {
                if assign {
                    assigns += u64::from(literals > 0);
                } else {
                    pins += literals;
                }
                (begun, assign, literals) = (false, false, 0);
            }
            b'1' if literal_at(s, i) => {
                literals += 1;
                i += 4;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    (assigns, pins)
}

/// The hazards of one run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Hazards {
    /// Marked cells used by the netlist: (mark, cell, instances), sorted. A cell with several
    /// marks appears once per mark.
    pub(crate) cells: Vec<(Mark, String, u64)>,
    /// `assign` statements with a literal constant, and cell pins tied to one.
    pub(crate) const_assigns: u64,
    pub(crate) const_pins: u64,
}

impl Hazards {
    pub(crate) fn is_empty(&self) -> bool {
        self.cells.is_empty() && self.const_assigns == 0 && self.const_pins == 0
    }

    /// Instances of cells with mark `m`.
    pub(crate) fn instances(&self, m: Mark) -> u64 {
        self.cells
            .iter()
            .filter(|(k, _, _)| *k == m)
            .map(|(_, _, n)| n)
            .sum()
    }
}

/// The hazards of a netlist from its cell counts (cell, instances), the marks of its library and
/// its text.
pub(crate) fn hazards(
    counts: &[(String, u64)],
    marks: &BTreeMap<String, Vec<Mark>>,
    netlist: &str,
) -> Hazards {
    let mut cells = Vec::new();
    for (cell, n) in counts {
        for m in marks.get(cell).into_iter().flatten() {
            cells.push((*m, cell.clone(), *n));
        }
        if cell.to_ascii_lowercase().contains("sparecell") {
            cells.push((Mark::SpareCell, cell.clone(), *n));
        }
    }
    cells.sort();
    let (const_assigns, const_pins) = literal_constants(netlist);
    Hazards {
        cells,
        const_assigns,
        const_pins,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIB: &str = r#"library ("demo") {
    /* cell ("commented") { is_isolation_cell : true; } */
    capacitive_load_unit (1, "pf");
    cell ("iso_1") {
        area : 3.75;
        is_isolation_cell : "true";
        pin ("A") { direction : "input"; }
    }
    cell (keep_1) {
        pin ("KAPWR") { always_on : true ; }
        is_level_shifter : "false";
        pin ("X") { function : "(A&!SLEEP)"; values("1, 2", \
            "3, 4"); }
    }
    cell ("ls_1") { is_level_shifter : "true"; always_on : "TRUE"; }
    cell ("plain") { area : 1; }
    always_on : true;
}
"#;

    // Marks come from the cell group or a nested group; false values, comments, attributes outside
    // cells and strings with separators are not marks.
    #[test]
    fn liberty_marks_by_cell() {
        let m = liberty_marks(LIB.as_bytes());
        let got: Vec<(&str, Vec<Mark>)> = m.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        assert_eq!(
            got,
            [
                ("iso_1", vec![Mark::Isolation]),
                ("keep_1", vec![Mark::AlwaysOn]),
                ("ls_1", vec![Mark::AlwaysOn, Mark::LevelShifter]),
            ]
        );
    }

    // SYNTH-REQ-0522, SYNTH-REQ-0601: constants in assigns count once per statement, constants on
    // pins once per pin; names and comments that contain the characters do not count, and a
    // comment before a statement does not change its kind.
    #[test]
    fn literal_constants_in_netlists() {
        let n = "module m(input a, output [3:0] y);\n  wire n1;\n  // assign y[0] = 1'b0;\n  \
                 /* 1'b1 */\n  tie_lo t0 (.LO(n1));\n  and2 g1 (.A(a), .B(1'b1), .X(y[0]));\n  \
                 mux2 g2 (.A0(1'B0), .A1(1'b1), .S(\\x1'b1 ), .X(y[1]));\n  assign y[2] = 1'b1;\n  \
                 assign y[3] = n1;\n  assign \\q1'b0  = a1'b0x;\nendmodule\n";
        assert_eq!(literal_constants(n), (1, 3));
        assert_eq!(literal_constants(""), (0, 0));
        let commented = "  wire w;\n  // a note; with a semicolon\n  /* x; */ assign y = 1'b0;\n  \
                         assignment_cell g (.A(1'b1));\n";
        assert_eq!(literal_constants(commented), (1, 1));
    }

    // Marked cells are counted per mark; a cell named like a spare cell is marked whatever the
    // library says.
    #[test]
    fn hazards_of_a_run() {
        let marks = liberty_marks(LIB.as_bytes());
        let counts = vec![
            ("iso_1".to_string(), 3),
            ("ls_1".to_string(), 2),
            ("plain".to_string(), 7),
            ("demo__macro_sparecell".to_string(), 1),
        ];
        let h = hazards(&counts, &marks, "assign y = 1'b0;\n");
        assert_eq!(h.instances(Mark::Isolation), 3);
        assert_eq!(h.instances(Mark::AlwaysOn), 2);
        assert_eq!(h.instances(Mark::LevelShifter), 2);
        assert_eq!(h.instances(Mark::SpareCell), 1);
        assert_eq!((h.const_assigns, h.const_pins), (1, 0));
        assert!(!h.is_empty());
        assert!(hazards(&counts[2..3], &marks, "").is_empty());
    }
}
