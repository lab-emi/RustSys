// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Lookup-table templates and tables (LIBERTY-REQ-0031, LIBERTY-REQ-0032): template variables,
//! per-table index overrides, and the checks that make a malformed table a cell problem
//! (LIBERTY-REQ-0057).

use std::collections::BTreeMap;

use rusteda_core::liberty::{Table, Template};

use super::diags::{Diags, code};
use super::syntax::{AttrValue, Group};

/// The kinds of template groups, in the order the reader enters them into the template map.
pub(super) const TEMPLATE_KINDS: [&str; 3] = [
    "lu_table_template",
    "power_lut_template",
    "output_current_template",
];

/// The numbers of the first complex attribute `name` of `g` (`index_1 ("1, 2")`, `values ("1, 2",
/// "3, 4")`) at the construct path `path`, with a warning for every text that is not a finite
/// number; such texts are skipped.
pub(super) fn numbers(g: &Group, name: &str, path: &str, diags: &mut Diags) -> Option<Vec<f64>> {
    let (texts, at) = g.attrs.iter().find_map(|a| match &a.value {
        AttrValue::Complex(v) if a.name == name => Some((v, (a.line, a.col))),
        _ => None,
    })?;
    let mut out = Vec::new();
    for t in texts {
        for tok in t
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|t| !t.is_empty())
        {
            match tok.parse::<f64>() {
                Ok(v) if v.is_finite() => out.push(v),
                _ => diags.warn(code::NOT_A_NUMBER, &format!("{path}/{name}"), at, || {
                    format!("'{tok}' is not a finite number; it is skipped")
                }),
            }
        }
    }
    Some(out)
}

/// The template groups of the library group `g`, by name (LIBERTY-REQ-0031). A name defined
/// twice keeps the definition entered last, with a warning; an unknown variable is kept with a
/// warning.
pub(super) fn templates(g: &Group, diags: &mut Diags) -> BTreeMap<String, Template> {
    let mut out: BTreeMap<String, Template> = BTreeMap::new();
    for kind in TEMPLATE_KINDS {
        let path = format!("library/{kind}");
        for t in g.children(kind) {
            let tpl = Template {
                variable_1: t.simple("variable_1").map(str::to_string),
                variable_2: t.simple("variable_2").map(str::to_string),
                variable_3: t.simple("variable_3").map(str::to_string),
                index_1: numbers(t, "index_1", &path, diags).unwrap_or_default(),
                index_2: numbers(t, "index_2", &path, diags).unwrap_or_default(),
                index_3: numbers(t, "index_3", &path, diags).unwrap_or_default(),
            };
            for axis in 1..=3 {
                if let Some(v) = tpl.variable(axis).filter(|v| !v.is_known()) {
                    let name = format!("variable_{axis}");
                    let a = t.attr(&name).map_or((t.line, t.col), |a| (a.line, a.col));
                    diags.warn(code::UNKNOWN_VALUE, &format!("{path}/{name}"), a, || {
                        format!("unknown table variable '{v}'")
                    });
                }
            }
            let Some(name) = t.args.first() else {
                diags.warn(code::UNKNOWN_VALUE, &path, (t.line, t.col), || {
                    "template without a name is ignored".to_string()
                });
                continue;
            };
            if out.contains_key(name) {
                diags.warn(code::DUPLICATE, &path, (t.line, t.col), || {
                    format!(
                        "template '{name}' is defined more than once; the last definition is used"
                    )
                });
            }
            out.insert(name.clone(), tpl);
        }
    }
    out
}

/// One table group `t` (a `cell_rise`, `rise_power`, ...) under the construct path `path`,
/// whose problems (LIBERTY-REQ-0057) are returned as (code, text) pairs.
pub(super) fn table(
    templates: &BTreeMap<String, Template>,
    t: &Group,
    path: &str,
    diags: &mut Diags,
) -> (Table, Vec<(&'static str, String)>) {
    let template = t.args.first().cloned().unwrap_or_default();
    let tpl = templates.get(&template);
    let mut problems = Vec::new();
    if tpl.is_none() && template != "scalar" {
        problems.push((
            code::NO_TEMPLATE,
            format!("{}: template '{template}' is not defined", t.kind),
        ));
    }
    let index = |k: usize, own: Option<Vec<f64>>| -> Vec<f64> {
        own.or_else(|| {
            tpl.map(|p| match k {
                1 => p.index_1.clone(),
                2 => p.index_2.clone(),
                _ => p.index_3.clone(),
            })
        })
        .unwrap_or_default()
    };
    let own_1 = numbers(t, "index_1", path, diags);
    let own_2 = numbers(t, "index_2", path, diags);
    let own_3 = numbers(t, "index_3", path, diags);
    let table = Table {
        variable_1: tpl.and_then(|p| p.variable_1.clone()),
        variable_2: tpl.and_then(|p| p.variable_2.clone()),
        variable_3: tpl.and_then(|p| p.variable_3.clone()),
        index_1: index(1, own_1),
        index_2: index(2, own_2),
        index_3: index(3, own_3),
        values: numbers(t, "values", path, diags).unwrap_or_default(),
        template,
    };
    if !table.is_consistent3() {
        let (n1, n2, n3) = table.shape3();
        problems.push((
            code::TABLE_SIZE,
            format!(
                "{}: {} values for {n1} x {n2} x {n3} index points",
                t.kind,
                table.values.len()
            ),
        ));
    }
    for (k, idx) in [&table.index_1, &table.index_2, &table.index_3]
        .into_iter()
        .enumerate()
    {
        if idx.windows(2).any(|w| w[1] <= w[0]) {
            problems.push((
                code::TABLE_INDEX,
                format!("{}: index_{} is not strictly increasing", t.kind, k + 1),
            ));
        }
    }
    (table, problems)
}
