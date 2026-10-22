// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Flattening of the top model (BLIF-REQ-0040 to BLIF-REQ-0042, BLIF-REQ-0060) and the driver
//! checks of the flat network (BLIF-REQ-0022, BLIF-REQ-0034).

use std::collections::HashMap;

use super::parse::{INPUT, Item, Models, OUTPUT};
use super::{
    COLLISION, CellInst, DRIVERS, Diag, INSTANCE, LIMIT, Latch, Loc, MAX_ERRORS, Network, Node,
    Options, RECURSION, TOP, UNDRIVEN, UNSUPPORTED,
};

/// One model instance being walked.
struct Frame {
    model: usize,
    /// Global signal of each local signal of the model.
    map: Vec<u32>,
    next: usize,
    path: String,
}

/// The network being built, with its counts against the limits.
struct Flat<'a> {
    net: Network,
    index: HashMap<String, u32>,
    items: usize,
    refs: usize,
    opts: &'a Options,
}

impl Flat<'_> {
    fn err(&self, at: Loc, code: &str, msg: impl Into<String>) -> Diag {
        super::err(&self.net.files, at, code, msg)
    }

    /// Counts one more item with `refs` signal references against the limits (BLIF-REQ-0060).
    fn count(&mut self, refs: usize, at: Loc) -> Result<(), Diag> {
        self.items += 1;
        self.refs = self.refs.saturating_add(refs);
        if self.items > self.opts.limits.max_items {
            return Err(self.err(
                at,
                LIMIT,
                format!(
                    "the flattened network has more than {} nodes, latches and cells",
                    self.opts.limits.max_items
                ),
            ));
        }
        self.check_refs(at)
    }

    fn check_refs(&self, at: Loc) -> Result<(), Diag> {
        if self.refs > self.opts.limits.max_refs {
            return Err(self.err(
                at,
                LIMIT,
                format!(
                    "the flattened network has more than {} signals and signal references",
                    self.opts.limits.max_refs
                ),
            ));
        }
        Ok(())
    }

    /// A new signal named `name`; a name already taken is a collision (BLIF-REQ-0041).
    fn signal(&mut self, name: String, at: Loc) -> Result<u32, Diag> {
        if self.index.contains_key(&name) {
            return Err(self.err(
                at,
                COLLISION,
                format!("the flattened name {name} is also the name of another signal"),
            ));
        }
        self.refs = self.refs.saturating_add(1);
        self.check_refs(at)?;
        let s = self.net.signals.len() as u32;
        self.index.insert(name.clone(), s);
        self.net.signals.push(name);
        Ok(s)
    }
}

/// Flattens the top model of `models` (BLIF-REQ-0040 to BLIF-REQ-0042).
pub(super) fn flatten(
    models: &Models,
    files: Vec<String>,
    opts: &Options,
) -> Result<(Network, Option<Diag>), Vec<Diag>> {
    let first = Loc {
        file: 0,
        line: 1,
        col: 1,
    };
    let top = match &opts.top {
        Some(t) => models.by_name.get(t).copied().ok_or_else(|| {
            vec![super::err(
                &files,
                first,
                TOP,
                format!("no model is named {t}"),
            )]
        })?,
        None if models.list.is_empty() => {
            return Err(vec![super::err(&files, first, TOP, "no model")]);
        }
        None => 0,
    };
    let tm = &models.list[top];
    let mut f = Flat {
        net: Network {
            name: tm.name.clone(),
            files,
            ..Network::default()
        },
        index: HashMap::new(),
        items: 0,
        refs: 0,
        opts,
    };
    if let Some(at) = tm.blackbox {
        return Err(vec![f.err(
            at,
            UNSUPPORTED,
            format!(
                "top model {} is a black box (.blackbox) without a function",
                tm.name
            ),
        )]);
    }
    let mut map = Vec::with_capacity(tm.names.len());
    for n in &tm.names {
        map.push(f.signal(n.clone(), tm.at).map_err(|d| vec![d])?);
    }
    f.net.inputs = tm
        .inputs
        .iter()
        .chain(&tm.clocks)
        .map(|&s| map[s as usize])
        .collect();
    f.net.clocks = tm.clocks.iter().map(|&s| map[s as usize]).collect();
    f.net.outputs = tm.outputs.iter().map(|&(s, _)| map[s as usize]).collect();
    f.net.output_locs = tm.outputs.iter().map(|&(_, a)| a).collect();
    walk(models, &mut f, top, map).map_err(|d| vec![d])?;
    let (errors, warning) = drivers(&f.net);
    if errors.is_empty() {
        Ok((f.net, warning))
    } else {
        Err(errors)
    }
}

/// The depth-first walk of BLIF-REQ-0002 with an explicit stack.
fn walk(models: &Models, f: &mut Flat<'_>, top: usize, map: Vec<u32>) -> Result<(), Diag> {
    let mut on_stack = vec![false; models.list.len()];
    on_stack[top] = true;
    let mut stack = vec![Frame {
        model: top,
        map,
        next: 0,
        path: String::new(),
    }];
    while let Some(fr) = stack.last_mut() {
        let m = &models.list[fr.model];
        let Some(item) = m.items.get(fr.next) else {
            on_stack[fr.model] = false;
            stack.pop();
            continue;
        };
        fr.next += 1;
        let g = |s: &u32| fr.map[*s as usize];
        match item {
            Item::Names {
                inputs,
                output,
                cover,
                at,
            } => {
                let node = Node {
                    inputs: inputs.iter().map(g).collect(),
                    output: g(output),
                    cover: cover.clone(),
                    at: *at,
                };
                f.count(inputs.len(), *at)?;
                f.net.nodes.push(node);
            }
            Item::Latch {
                input,
                output,
                kind,
                control,
                init,
                at,
            } => {
                let l = Latch {
                    input: g(input),
                    output: g(output),
                    kind: *kind,
                    control: control.as_ref().map(g),
                    init: *init,
                    at: *at,
                };
                f.count(2, *at)?;
                f.net.latches.push(l);
            }
            Item::Cell {
                cell,
                conns,
                mlatch,
                at,
            } => {
                let c = CellInst {
                    cell: cell.clone(),
                    conns: conns.iter().map(|(p, s)| (p.clone(), g(s))).collect(),
                    mlatch: mlatch.map(|(c, i)| (c.as_ref().map(g), i)),
                    at: *at,
                };
                f.count(conns.len(), *at)?;
                f.net.cells.push(c);
            }
            Item::Subckt {
                model,
                ordinal,
                conns,
                at,
            } => {
                let Some(&child) = models.by_name.get(model) else {
                    return Err(f.err(*at, INSTANCE, format!("unknown model {model}")));
                };
                if on_stack[child] {
                    return Err(f.err(
                        *at,
                        RECURSION,
                        format!(
                            "model {model} instantiates itself, directly or through other models"
                        ),
                    ));
                }
                let cm = &models.list[child];
                if cm.blackbox.is_some() {
                    return Err(f.err(
                        *at,
                        UNSUPPORTED,
                        format!("model {model} is a black box (.blackbox) without a function"),
                    ));
                }
                let inst = format!("{model}_{ordinal}");
                let path = if fr.path.is_empty() {
                    inst
                } else {
                    format!("{}.{inst}", fr.path)
                };
                let mut cmap = vec![u32::MAX; cm.names.len()];
                for (formal, actual, a) in conns {
                    let port = cm
                        .index
                        .get(formal)
                        .copied()
                        .filter(|&l| cm.flags[l as usize] & (INPUT | OUTPUT) != 0);
                    let Some(l) = port else {
                        return Err(f.err(
                            *a,
                            INSTANCE,
                            format!("{formal} is not a port of model {model}"),
                        ));
                    };
                    cmap[l as usize] = fr.map[*actual as usize];
                }
                if let Some(&l) = cm
                    .inputs
                    .iter()
                    .chain(&cm.clocks)
                    .find(|&&l| cmap[l as usize] == u32::MAX)
                {
                    return Err(f.err(
                        *at,
                        INSTANCE,
                        format!(
                            "input {} of model {model} is not connected",
                            cm.names[l as usize]
                        ),
                    ));
                }
                for (l, n) in cm.names.iter().enumerate() {
                    if cmap[l] == u32::MAX {
                        cmap[l] = f.signal(format!("{path}.{n}"), *at)?;
                    }
                }
                on_stack[child] = true;
                stack.push(Frame {
                    model: child,
                    map: cmap,
                    next: 0,
                    path,
                });
            }
        }
    }
    Ok(())
}

/// The driver checks of BLIF-REQ-0022 on the flat network: signals connected to cells count as
/// driven and are not checked for a second driver (BLIF-REQ-0034). Returns the errors (second
/// drivers) and the warning that names the signals without a driver.
fn drivers(net: &Network) -> (Vec<Diag>, Option<Diag>) {
    const NONE: u8 = 0;
    let n = net.signals.len();
    let mut driver = vec![NONE; n];
    let mut errors = Vec::new();
    for &s in &net.inputs {
        driver[s as usize] = 1;
    }
    let mut drive = |errors: &mut Vec<Diag>, s: u32, at: Loc, by: u8| {
        if driver[s as usize] != NONE && errors.len() < MAX_ERRORS {
            errors.push(super::err(
                &net.files,
                at,
                DRIVERS,
                format!(
                    "signal {} has more than one driver",
                    net.signals[s as usize]
                ),
            ));
        }
        driver[s as usize] = by;
    };
    for nd in &net.nodes {
        drive(&mut errors, nd.output, nd.at, 2);
    }
    for l in &net.latches {
        drive(&mut errors, l.output, l.at, 3);
    }
    let mut seen = vec![false; n];
    for c in &net.cells {
        for (_, s) in &c.conns {
            seen[*s as usize] = true;
        }
    }
    // Signals without a driver, in the order they are read: one warning for all.
    let mut undriven: Vec<(u32, Loc)> = Vec::new();
    let mut read = |s: u32, at: Loc| {
        if driver[s as usize] == NONE && !seen[s as usize] {
            seen[s as usize] = true;
            undriven.push((s, at));
        }
    };
    for nd in &net.nodes {
        for &s in &nd.inputs {
            read(s, nd.at);
        }
    }
    for l in &net.latches {
        read(l.input, l.at);
        if let Some(c) = l.control {
            read(c, l.at);
        }
    }
    for c in &net.cells {
        if let Some((Some(s), _)) = c.mlatch {
            read(s, c.at);
        }
    }
    for (&s, &at) in net.outputs.iter().zip(&net.output_locs) {
        read(s, at);
    }
    let warning = undriven.first().map(|&(_, at)| {
        let names: Vec<&str> = undriven
            .iter()
            .take(8)
            .map(|&(s, _)| net.signals[s as usize].as_str())
            .collect();
        let file = net
            .files
            .get(at.file as usize)
            .map_or("<blif>", String::as_str);
        Diag::warning(
            UNDRIVEN,
            format!(
                "{} signals are read or are outputs but have no driver: {}{}",
                undriven.len(),
                names.join(", "),
                if undriven.len() > names.len() {
                    ", ..."
                } else {
                    ""
                }
            ),
        )
        .at(file, at.line.max(1), at.col.max(1))
    });
    (errors, warning)
}
