// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Table models (PROTO-REQ-0130): the rows behind `rustsys show`. This module holds the row
//! contract and its text, TSV and JSON renderings; the models of each product live in the
//! submodules and are re-exported here.

mod checks;
mod synthesis;

use std::fmt::Write as _;

use crate::proto::format::{MISSING, Unit, exact, name, number};

pub use checks::MismatchRows;
pub use synthesis::{CellRows, PhaseRows, UnusedRows};

/// A cell value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value<'a> {
    Text(&'a str),
    Num(f64),
    Missing,
}

/// Column alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// A column: key (for filters, sorting and TSV), title, unit and alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnSpec {
    pub key: &'static str,
    pub title: &'static str,
    pub unit: Unit,
    pub align: Align,
}

const fn text_col(key: &'static str, title: &'static str) -> ColumnSpec {
    ColumnSpec {
        key,
        title,
        unit: Unit::Plain,
        align: Align::Left,
    }
}

const fn num_col(key: &'static str, title: &'static str, unit: Unit) -> ColumnSpec {
    ColumnSpec {
        key,
        title,
        unit,
        align: Align::Right,
    }
}

/// The one row contract (PROTO-REQ-0130): O(1) per cell, no allocation.
pub trait TableModel: Send + Sync + std::fmt::Debug {
    fn columns(&self) -> &[ColumnSpec];
    fn len(&self) -> usize;
    fn value(&self, row: usize, col: usize) -> Value<'_>;
    /// The column that names the row (filter words match it).
    fn name_column(&self) -> usize {
        0
    }
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Writes the displayed text of a cell into `out` (PROTO-REQ-0100).
    fn cell(&self, row: usize, col: usize, out: &mut String) {
        out.clear();
        match self.value(row, col) {
            Value::Text(t) => out.push_str(&name(t)),
            Value::Num(v) if v.is_nan() => out.push_str(MISSING),
            Value::Num(v) => out.push_str(&number(v, self.columns()[col].unit)),
            Value::Missing => out.push_str(MISSING),
        }
    }
    /// Column index of a key.
    fn column(&self, key: &str) -> Option<usize> {
        self.columns().iter().position(|c| c.key == key)
    }
}

/// The keys of a model, for messages.
pub fn keys(m: &dyn TableModel) -> String {
    m.columns()
        .iter()
        .map(|c| c.key)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Rows as tab-separated text with a header of column keys and exact numbers (CLI-REQ-0102).
pub fn tsv(m: &dyn TableModel, order: &[u32]) -> String {
    let mut out = String::new();
    let cols = m.columns();
    let _ = writeln!(
        out,
        "{}",
        cols.iter().map(|c| c.key).collect::<Vec<_>>().join("\t")
    );
    for &r in order {
        let row: Vec<String> = (0..cols.len())
            .map(|c| match m.value(r as usize, c) {
                Value::Text(t) => name(t).replace('\t', " "),
                Value::Num(v) if v.is_nan() => String::new(),
                Value::Num(v) => exact(v),
                Value::Missing => String::new(),
            })
            .collect();
        let _ = writeln!(out, "{}", row.join("\t"));
    }
    out
}

/// Rows as JSON objects keyed by column key (CLI-REQ-0102).
pub fn json_rows(m: &dyn TableModel, order: &[u32]) -> serde_json::Value {
    let cols = m.columns();
    serde_json::Value::Array(
        order
            .iter()
            .map(|&r| {
                let mut o = serde_json::Map::new();
                for (c, spec) in cols.iter().enumerate() {
                    let v = match m.value(r as usize, c) {
                        Value::Text(t) => serde_json::Value::String(t.to_string()),
                        Value::Num(v) if v.is_finite() => serde_json::json!(v),
                        _ => serde_json::Value::Null,
                    };
                    o.insert(spec.key.to_string(), v);
                }
                serde_json::Value::Object(o)
            })
            .collect(),
    )
}

/// Aligned text of rows, with a header of titles.
pub fn text(m: &dyn TableModel, order: &[u32]) -> String {
    let cols = m.columns();
    let mut cells: Vec<Vec<String>> = Vec::with_capacity(order.len() + 1);
    cells.push(
        cols.iter()
            .map(|c| match c.unit {
                Unit::Ns => format!("{} (ns)", c.title),
                Unit::Seconds => format!("{} (s)", c.title),
                Unit::Percent => format!("{} (%)", c.title),
                _ => c.title.to_string(),
            })
            .collect(),
    );
    let mut buf = String::new();
    for &r in order {
        cells.push(
            (0..cols.len())
                .map(|c| {
                    m.cell(r as usize, c, &mut buf);
                    buf.clone()
                })
                .collect(),
        );
    }
    let widths: Vec<usize> = (0..cols.len())
        .map(|c| {
            cells
                .iter()
                .map(|r| r[c].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = String::new();
    for row in &cells {
        let mut line = String::new();
        for (c, v) in row.iter().enumerate() {
            if c > 0 {
                line.push_str("  ");
            }
            let pad = widths[c] - v.chars().count();
            match cols[c].align {
                Align::Left => {
                    line.push_str(v);
                    if c + 1 < cols.len() {
                        line.extend(std::iter::repeat_n(' ', pad));
                    }
                }
                Align::Right => {
                    line.extend(std::iter::repeat_n(' ', pad));
                    line.push_str(v);
                }
            }
        }
        let _ = writeln!(out, "{}", line.trim_end());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustsys::view as synth;

    // PROTO-REQ-0100: a missing value (a cell without a recorded area) shows as `—`; TSV gives
    // an empty field and JSON `null`.
    #[test]
    fn missing_values() {
        let r = synth::Report {
            cells: [("inv".to_string(), 2)].into(),
            area: 3.0,
            ..Default::default()
        };
        let m = CellRows::new(&r);
        let mut s = String::new();
        m.cell(0, 1, &mut s);
        assert_eq!(s, "2");
        m.cell(0, 2, &mut s);
        assert_eq!(s, "—");
        assert_eq!(
            tsv(&m, &[0]),
            "cell\tcount\tarea_each\tarea_total\tshare\ninv\t2\t\t\t\n"
        );
        assert_eq!(json_rows(&m, &[0])[0]["area_each"], serde_json::Value::Null);
        let shown = text(&m, &[0]);
        let row: Vec<&str> = shown.lines().nth(1).unwrap().split_whitespace().collect();
        assert_eq!(row, ["inv", "2", "—", "—", "—"]);
    }
}
