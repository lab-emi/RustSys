// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Lookup tables (LIBERTY-REQ-0013, LIBERTY-REQ-0030, LIBERTY-REQ-0032 to LIBERTY-REQ-0034).

use super::kinds::TableVariable;

/// A lookup table with up to three axes. `values` is row-major over (index_1, index_2,
/// index_3): the value at points (i, j, k) is `values[(i * n2 + j) * n3 + k]`, where n2 and n3
/// are the lengths of `index_2` and `index_3`, each at least 1 (LIBERTY-REQ-0032).
///
/// `shape`, `is_consistent`, `lookup`, `lookup_slew_load` and `mid_slew` are the two-axis forms:
/// a table whose third axis has more than one point is not consistent for them. `shape3`,
/// `is_consistent3` and `lookup3` cover all three axes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Table {
    pub template: String,
    pub variable_1: Option<String>,
    pub variable_2: Option<String>,
    pub index_1: Vec<f64>,
    pub index_2: Vec<f64>,
    pub values: Vec<f64>,
    pub variable_3: Option<String>,
    pub index_3: Vec<f64>,
}

/// Interpolation weights along one axis: (lower index, upper index, fraction).
fn axis(index: &[f64], x: f64) -> (usize, usize, f64) {
    match index.len() {
        0 | 1 => (0, 0, 0.0),
        n => {
            // Segment containing x, clamped to the first/last segment for extrapolation.
            let mut k = 0;
            while k + 2 < n && x > index[k + 1] {
                k += 1;
            }
            let (x0, x1) = (index[k], index[k + 1]);
            let t = if x1 == x0 { 0.0 } else { (x - x0) / (x1 - x0) };
            (k, k + 1, t)
        }
    }
}

/// The point at fraction `t` from `a` to `b`.
fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

impl Table {
    /// Number of rows and columns.
    pub fn shape(&self) -> (usize, usize) {
        (self.index_1.len().max(1), self.index_2.len().max(1))
    }

    /// True if the value count matches the indices.
    pub fn is_consistent(&self) -> bool {
        let (r, c) = self.shape();
        !self.values.is_empty() && (self.values.len() == r * c || self.values.len() == 1)
    }

    fn at(&self, i: usize, j: usize) -> f64 {
        if self.values.len() == 1 {
            return self.values[0];
        }
        let (_, c) = self.shape();
        self.values.get(i * c + j).copied().unwrap_or(0.0)
    }

    /// Bilinear interpolation or linear extrapolation at (x1, x2) (LIBERTY-REQ-0030).
    pub fn lookup(&self, x1: f64, x2: f64) -> f64 {
        if !self.is_consistent() {
            return 0.0;
        }
        let (i0, i1, t) = axis(&self.index_1, x1);
        let (j0, j1, u) = axis(&self.index_2, x2);
        let v00 = self.at(i0, j0);
        let v01 = self.at(i0, j1);
        let v10 = self.at(i1, j0);
        let v11 = self.at(i1, j1);
        let a = v00 + (v01 - v00) * u;
        let b = v10 + (v11 - v10) * u;
        a + (b - a) * t
    }

    /// Looks up by named variables, using the template variable names to decide which index
    /// holds the input transition and which the output load.
    pub fn lookup_slew_load(&self, slew: f64, load: f64) -> f64 {
        let is_load = |v: &Option<String>| v.as_deref().is_some_and(|s| s.contains("capacitance"));
        if is_load(&self.variable_1) && !is_load(&self.variable_2) {
            self.lookup(load, slew)
        } else {
            self.lookup(slew, load)
        }
    }

    /// Index value in the middle of the slew axis (LIBERTY-REQ-0034).
    pub fn mid_slew(&self) -> f64 {
        let is_load = |v: &Option<String>| v.as_deref().is_some_and(|s| s.contains("capacitance"));
        let idx = if is_load(&self.variable_1) && !is_load(&self.variable_2) {
            &self.index_2
        } else {
            &self.index_1
        };
        if idx.is_empty() {
            0.0
        } else {
            idx[idx.len() / 2]
        }
    }

    /// The variable of axis `axis` (1 to 3), when the table has one (LIBERTY-REQ-0032).
    pub fn variable(&self, axis: usize) -> Option<TableVariable> {
        let v = match axis {
            1 => &self.variable_1,
            2 => &self.variable_2,
            3 => &self.variable_3,
            _ => return None,
        };
        v.as_deref().map(|s| TableVariable::from_name(s.trim()))
    }

    /// Number of points of the three axes, each at least 1 (LIBERTY-REQ-0032).
    pub fn shape3(&self) -> (usize, usize, usize) {
        (
            self.index_1.len().max(1),
            self.index_2.len().max(1),
            self.index_3.len().max(1),
        )
    }

    /// True if the value count is the product of the three axis lengths, or 1 for a constant
    /// table (LIBERTY-REQ-0032).
    pub fn is_consistent3(&self) -> bool {
        let (n1, n2, n3) = self.shape3();
        !self.values.is_empty() && (self.values.len() == n1 * n2 * n3 || self.values.len() == 1)
    }

    fn at3(&self, i: usize, j: usize, k: usize) -> f64 {
        if self.values.len() == 1 {
            return self.values[0];
        }
        let (_, n2, n3) = self.shape3();
        self.values
            .get((i * n2 + j) * n3 + k)
            .copied()
            .unwrap_or(0.0)
    }

    /// Interpolation between the enclosing index points and linear extrapolation beyond the
    /// index range, along every axis with at least two points, at (x1, x2, x3); 0 for a table
    /// that is not consistent (LIBERTY-REQ-0033). On a table of finite values without a third
    /// axis it equals [`Table::lookup`] bit for bit.
    pub fn lookup3(&self, x1: f64, x2: f64, x3: f64) -> f64 {
        if !self.is_consistent3() {
            return 0.0;
        }
        let (i0, i1, t) = axis(&self.index_1, x1);
        let (j0, j1, u) = axis(&self.index_2, x2);
        let (k0, k1, w) = axis(&self.index_3, x3);
        let along3 = |i: usize, j: usize| lerp(self.at3(i, j, k0), self.at3(i, j, k1), w);
        let a = lerp(along3(i0, j0), along3(i0, j1), u);
        let b = lerp(along3(i1, j0), along3(i1, j1), u);
        lerp(a, b, t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // LIBERTY-REQ-0030
    #[test]
    fn interpolation_and_extrapolation() {
        let t = Table {
            index_1: vec![1.0, 2.0],
            index_2: vec![10.0, 20.0, 40.0],
            values: vec![1.0, 2.0, 4.0, 3.0, 4.0, 6.0],
            ..Table::default()
        };
        assert!((t.lookup(1.0, 10.0) - 1.0).abs() < 1e-12);
        assert!((t.lookup(2.0, 40.0) - 6.0).abs() < 1e-12);
        assert!((t.lookup(1.5, 15.0) - 2.5).abs() < 1e-12);
        // Extrapolation beyond both ends.
        assert!((t.lookup(3.0, 10.0) - 5.0).abs() < 1e-12);
        assert!((t.lookup(1.0, 0.0) - 0.0).abs() < 1e-12);
        assert!((t.lookup(1.0, 60.0) - 6.0).abs() < 1e-12);
        let s = Table {
            values: vec![7.0],
            ..Table::default()
        };
        assert_eq!(s.lookup(5.0, 5.0), 7.0);
    }

    // LIBERTY-REQ-0034: the middle index point of the input-transition axis, which is `index_2`
    // when `variable_1` names a capacitance and `variable_2` does not, otherwise `index_1`; the
    // upper of the two middle points of an even count; 0 without index points.
    #[test]
    fn middle_of_the_transition_axis() {
        let slew = Some("input_net_transition");
        let load = Some("total_output_net_capacitance");
        let t = |v1: Option<&str>, v2: Option<&str>| Table {
            variable_1: v1.map(String::from),
            variable_2: v2.map(String::from),
            index_1: vec![0.1, 0.2, 0.4],
            index_2: vec![1.0, 2.0, 4.0, 8.0],
            ..Table::default()
        };
        assert_eq!(t(slew, load).mid_slew(), 0.2);
        assert_eq!(t(None, None).mid_slew(), 0.2);
        assert_eq!(t(load, load).mid_slew(), 0.2);
        assert_eq!(t(load, slew).mid_slew(), 4.0);
        assert_eq!(t(load, None).mid_slew(), 4.0);
        let one = Table {
            index_1: vec![0.3],
            ..Table::default()
        };
        assert_eq!(one.mid_slew(), 0.3);
        assert_eq!(Table::default().mid_slew(), 0.0);
        let no_slew_points = Table {
            index_2: Vec::new(),
            ..t(load, slew)
        };
        assert_eq!(no_slew_points.mid_slew(), 0.0);
    }

    /// A 2 x 3 x 2 table of v(x1, x2, x3) = 100 x1 + 10 x2 + x3 at its index points, which every
    /// trilinear interpolation and extrapolation reproduces exactly.
    fn affine() -> Table {
        let (i1, i2, i3) = (vec![1.0, 2.0], vec![1.0, 2.0, 4.0], vec![0.5, 1.5]);
        let mut values = Vec::new();
        for a in &i1 {
            for b in &i2 {
                for c in &i3 {
                    values.push(100.0 * a + 10.0 * b + c);
                }
            }
        }
        Table {
            index_1: i1,
            index_2: i2,
            index_3: i3,
            values,
            ..Table::default()
        }
    }

    // LIBERTY-REQ-0032, LIBERTY-REQ-0033: row-major value layout over three axes, interpolation
    // and extrapolation along each, constant tables, and the two-axis forms.
    #[test]
    fn three_axes() {
        let t = affine();
        assert_eq!(t.shape3(), (2, 3, 2));
        assert!(t.is_consistent3());
        // Layout: (i, j, k) = (1, 2, 0) is value index (1 * 3 + 2) * 2 + 0 = 10.
        assert_eq!(t.values[10], 200.0 + 40.0 + 0.5);
        let f = |a: f64, b: f64, c: f64| 100.0 * a + 10.0 * b + c;
        for (a, b, c) in [
            (1.0, 1.0, 0.5),
            (2.0, 4.0, 1.5),
            (1.25, 3.0, 1.0),
            (0.0, 0.0, 0.0),
            (3.0, 8.0, 2.5),
            (1.5, 1.5, -1.0),
        ] {
            assert!(
                (t.lookup3(a, b, c) - f(a, b, c)).abs() < 1e-9,
                "{a} {b} {c}"
            );
        }
        // A third axis of more than one point fails the two-axis check.
        assert!(!t.is_consistent());
        assert_eq!(t.lookup(1.0, 1.0), 0.0);
        let bad = Table {
            values: vec![1.0, 2.0, 3.0],
            ..affine()
        };
        assert!(!bad.is_consistent3());
        assert_eq!(bad.lookup3(1.0, 1.0, 1.0), 0.0);
        let constant = Table {
            values: vec![7.0],
            ..affine()
        };
        assert_eq!(constant.lookup3(9.0, 9.0, 9.0), 7.0);
        assert!(!Table::default().is_consistent3());
    }

    // LIBERTY-REQ-0033: without a third axis, lookup3 equals lookup bit for bit.
    #[test]
    fn two_axis_tables_agree() {
        let t = Table {
            index_1: vec![0.01, 0.1, 0.7],
            index_2: vec![0.001, 0.02, 0.3],
            values: vec![0.1, 0.13, 0.9, 0.17, 0.25, 1.1, 0.33, 0.6, 1.9],
            ..Table::default()
        };
        for (a, b) in [
            (0.05, 0.01),
            (0.0, 0.0),
            (1.0, 0.5),
            (0.7, 0.02),
            (0.33, 0.123),
        ] {
            for c in [0.0, 5.0] {
                assert_eq!(t.lookup3(a, b, c).to_bits(), t.lookup(a, b).to_bits());
            }
        }
    }

    // LIBERTY-REQ-0032: typed axis variables.
    #[test]
    fn axis_variables() {
        let t = Table {
            variable_1: Some("input_net_transition".into()),
            variable_2: Some("total_output_net_capacitance".into()),
            variable_3: Some("my_axis".into()),
            ..Table::default()
        };
        assert_eq!(t.variable(1), Some(TableVariable::InputNetTransition));
        assert_eq!(
            t.variable(2),
            Some(TableVariable::TotalOutputNetCapacitance)
        );
        assert_eq!(t.variable(3), Some(TableVariable::Other("my_axis".into())));
        assert_eq!(t.variable(4), None);
    }
}
