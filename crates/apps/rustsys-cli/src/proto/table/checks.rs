// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Table models of the synthesis check (PROTO-REQ-0130): its mismatches.

use rustsys_check::CheckReport;

use super::{ColumnSpec, TableModel, Value, text_col};

/// Mismatches of a synthesis check.
#[derive(Debug, Clone)]
pub struct MismatchRows(Vec<(String, String)>);

const MISMATCH_COLS: &[ColumnSpec] = &[text_col("name", "Name"), text_col("kind", "Kind")];

impl MismatchRows {
    pub fn new(r: &CheckReport) -> MismatchRows {
        MismatchRows(
            r.mismatches
                .iter()
                .map(|m| (m.name.clone(), m.kind.clone()))
                .collect(),
        )
    }
}

impl TableModel for MismatchRows {
    fn columns(&self) -> &[ColumnSpec] {
        MISMATCH_COLS
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
