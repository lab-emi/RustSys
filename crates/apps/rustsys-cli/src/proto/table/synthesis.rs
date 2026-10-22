// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Table models of synthesis runs (PROTO-REQ-0130): cells used, cells not usable, phases.

use rustsys::view as synth;

use super::{ColumnSpec, TableModel, Value, num_col, text_col};
use crate::proto::format::Unit;

/// Library cells used by a synthesis run.
#[derive(Debug, Clone)]
pub struct CellRows {
    name: Vec<String>,
    count: Vec<f64>,
    area_each: Vec<f64>,
    area_total: Vec<f64>,
    share: Vec<f64>,
}

const CELL_COLS: &[ColumnSpec] = &[
    text_col("cell", "Cell"),
    num_col("count", "Count", Unit::Count),
    num_col("area_each", "Area each", Unit::Area),
    num_col("area_total", "Total area", Unit::Area),
    num_col("share", "Share", Unit::Percent),
];

impl CellRows {
    pub fn new(r: &synth::Report) -> CellRows {
        let mut m = CellRows {
            name: Vec::new(),
            count: Vec::new(),
            area_each: Vec::new(),
            area_total: Vec::new(),
            share: Vec::new(),
        };
        for (c, &n) in &r.cells {
            let each = r.cell_area.get(c).copied().unwrap_or(f64::NAN);
            let total = each * n as f64;
            m.name.push(c.clone());
            m.count.push(n as f64);
            m.area_each.push(each);
            m.area_total.push(total);
            m.share.push(if r.area > 0.0 {
                100.0 * total / r.area
            } else {
                f64::NAN
            });
        }
        m
    }

    /// Sum of the total areas (derived), NaN without per-cell areas.
    pub fn total(&self) -> f64 {
        self.area_total.iter().sum()
    }
}

impl TableModel for CellRows {
    fn columns(&self) -> &[ColumnSpec] {
        CELL_COLS
    }
    fn len(&self) -> usize {
        self.name.len()
    }
    fn value(&self, r: usize, c: usize) -> Value<'_> {
        match c {
            0 => Value::Text(&self.name[r]),
            1 => Value::Num(self.count[r]),
            2 => Value::Num(self.area_each[r]),
            3 => Value::Num(self.area_total[r]),
            _ => Value::Num(self.share[r]),
        }
    }
}

/// Library cells synthesis cannot use, with the reason.
#[derive(Debug, Clone)]
pub struct UnusedRows(Vec<(String, String)>);

const UNUSED_COLS: &[ColumnSpec] = &[text_col("cell", "Cell"), text_col("reason", "Reason")];

impl UnusedRows {
    pub fn new(r: &synth::Report) -> UnusedRows {
        UnusedRows(r.unused_cells.clone().into_iter().collect())
    }
}

impl TableModel for UnusedRows {
    fn columns(&self) -> &[ColumnSpec] {
        UNUSED_COLS
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn value(&self, r: usize, c: usize) -> Value<'_> {
        if c == 0 {
            Value::Text(&self.0[r].0)
        } else {
            Value::Text(&self.0[r].1)
        }
    }
}

/// Phases and their seconds.
#[derive(Debug, Clone)]
pub struct PhaseRows(pub Vec<(String, f64)>);

const PHASE_COLS: &[ColumnSpec] = &[
    text_col("phase", "Phase"),
    num_col("seconds", "Seconds", Unit::Seconds),
];

impl TableModel for PhaseRows {
    fn columns(&self) -> &[ColumnSpec] {
        PHASE_COLS
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn value(&self, r: usize, c: usize) -> Value<'_> {
        if c == 0 {
            Value::Text(&self.0[r].0)
        } else {
            Value::Num(self.0[r].1)
        }
    }
}
