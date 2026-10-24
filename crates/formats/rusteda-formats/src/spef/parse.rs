// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

use super::{Connection, Error, Limits, Net, Node, Resistor, Spef};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, PartialEq)]
enum Section {
    Header,
    Names,
    Ports,
    Conn,
    Cap,
    Res,
    End,
}

struct Parser {
    limits: Limits,
    line: usize,
    name_bytes: usize,
    names: BTreeMap<String, String>,
    net_names: BTreeSet<String>,
    node_names: BTreeMap<String, u32>,
    conn_nodes: BTreeSet<u32>,
    cap_ids: BTreeSet<u64>,
    cap_terms: Vec<(u32, f64)>,
    res_ids: BTreeSet<u64>,
    current: Option<Net>,
    section: Section,
    cap_scale: Option<f64>,
    res_scale: Option<f64>,
    header: BTreeSet<String>,
    spef: Spef,
}

/// Parse within explicit allocation/work budgets. No graph traversal or recursion.
pub fn read(text: &str, limits: Limits) -> Result<Spef, Error> {
    limits.validate()?;
    if text.len() > limits.max_bytes {
        return Err(Error::new(0, "T0072", "SPEF source byte budget exceeded"));
    }
    let mut p = Parser {
        limits,
        line: 0,
        name_bytes: 0,
        names: BTreeMap::new(),
        net_names: BTreeSet::new(),
        node_names: BTreeMap::new(),
        conn_nodes: BTreeSet::new(),
        cap_ids: BTreeSet::new(),
        cap_terms: Vec::new(),
        res_ids: BTreeSet::new(),
        current: None,
        section: Section::Header,
        cap_scale: None,
        res_scale: None,
        header: BTreeSet::new(),
        spef: Spef {
            design: String::new(),
            divider: '/',
            delimiter: ':',
            bus_delimiter: ('[', ']'),
            nets: Vec::new(),
            coupling_records: 0,
            metadata_records: 0,
            node_count: 0,
            element_count: 0,
        },
    };
    for (i, raw) in text.lines().enumerate() {
        p.line = i + 1;
        if raw.len() > 65536 {
            return Err(p.err("line exceeds 65536 bytes"));
        }
        let t = tokens(raw).map_err(|m| p.err(m))?;
        if !t.is_empty() {
            p.line(&t)?;
        }
    }
    if p.current.is_some() {
        return Err(p.err("missing *END"));
    }
    if p.spef.nets.is_empty() {
        return Err(p.err("no detailed RC nets"));
    }
    for h in [
        "*SPEF",
        "*DESIGN",
        "*DIVIDER",
        "*DELIMITER",
        "*BUS_DELIMITER",
        "*T_UNIT",
        "*C_UNIT",
        "*R_UNIT",
    ] {
        if !p.header.contains(h) {
            return Err(p.err(format!("missing {h}")));
        }
    }
    p.spef.nets.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(p.spef)
}

// Keep escapes while tokenizing: an escaped divider is a literal name character.
fn tokens(raw: &str) -> Result<Vec<String>, &'static str> {
    let mut out = Vec::new();
    let mut token = String::new();
    let mut quoted = false;
    let mut escape = false;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if escape {
            token.push(c);
            escape = false;
            continue;
        }
        if c == '\\' {
            token.push(c);
            escape = true;
            continue;
        }
        if c == '"' {
            quoted = !quoted;
            continue;
        }
        if !quoted && c == '/' && chars.peek() == Some(&'/') {
            break;
        }
        if !quoted && c.is_whitespace() {
            if !token.is_empty() {
                out.push(std::mem::take(&mut token));
            }
        } else {
            token.push(c);
        }
        if token.len() > 4096 || out.len() > 1024 {
            return Err("token/name budget exceeded");
        }
    }
    if quoted || escape {
        return Err("unterminated quote or escape");
    }
    if !token.is_empty() {
        out.push(token);
    }
    Ok(out)
}

impl Parser {
    fn err(&self, m: impl Into<String>) -> Error {
        Error::new(self.line, "T0071", m)
    }
    fn budget(&self, m: &str) -> Error {
        Error::new(self.line, "T0072", m)
    }
    fn number(&self, s: &str) -> Result<f64, Error> {
        let v: f64 = s
            .parse()
            .map_err(|_| self.err(format!("invalid scalar {s}")))?;
        if !v.is_finite() || v < 0.0 {
            return Err(self.err("expected finite nonnegative scalar"));
        }
        Ok(v)
    }
    fn scaled(&self, s: &str, scale: f64) -> Result<f64, Error> {
        let v = self.number(s)? * scale;
        if !v.is_finite() {
            return Err(self.err("unit conversion overflow"));
        }
        Ok(v)
    }
    fn name(&mut self, raw: &str) -> Result<String, Error> {
        let s = if raw.starts_with('*') {
            let n = raw.bytes().skip(1).take_while(u8::is_ascii_digit).count() + 1;
            let key = &raw[..n];
            let base = self
                .names
                .get(key)
                .ok_or_else(|| self.err(format!("unresolved name {key}")))?;
            format!("{base}{}", &raw[n..])
        } else {
            raw.to_owned()
        };
        if s.len() > 4096 || self.name_bytes.saturating_add(s.len()) > self.limits.max_name_bytes {
            return Err(self.budget("SPEF name byte budget exceeded"));
        }
        self.name_bytes += s.len();
        Ok(s)
    }
    fn node(&mut self, name: String) -> Result<u32, Error> {
        if let Some(&id) = self.node_names.get(&name) {
            return Ok(id);
        }
        if self.spef.node_count == self.limits.max_nodes {
            return Err(self.budget("RC node budget exceeded"));
        }
        let net = self.current.as_mut().expect("in net");
        let id = net.nodes.len() as u32;
        self.node_names.insert(name.clone(), id);
        net.nodes.push(Node { name, cap_pf: 0.0 });
        self.spef.node_count += 1;
        Ok(id)
    }
    fn element(&mut self) -> Result<(), Error> {
        if self.spef.element_count == self.limits.max_elements {
            return Err(self.budget("RC element budget exceeded"));
        }
        self.spef.element_count += 1;
        Ok(())
    }
    fn line(&mut self, t: &[String]) -> Result<(), Error> {
        let key = t[0].as_str();
        if key == "*D_NET" {
            if self.current.is_some() || t.len() != 3 {
                return Err(self.err("invalid *D_NET nesting/arity"));
            }
            if self.spef.nets.len() == self.limits.max_nets {
                return Err(self.budget("RC net budget exceeded"));
            }
            let scale = self
                .cap_scale
                .ok_or_else(|| self.err("missing capacitance unit"))?;
            if self.res_scale.is_none() {
                return Err(self.err("missing resistance unit"));
            }
            let name = self.name(&t[1])?;
            if !self.net_names.insert(name.clone()) {
                return Err(self.err("duplicate *D_NET"));
            }
            let cap = self.scaled(&t[2], scale)?;
            self.current = Some(Net {
                name,
                total_cap_pf: cap,
                nodes: Vec::new(),
                connections: Vec::new(),
                resistors: Vec::new(),
            });
            self.section = Section::End;
            return Ok(());
        }
        if self.current.is_some() {
            return self.net_line(t);
        }
        if ["*CONN", "*CAP", "*RES", "*END"].contains(&key) {
            return Err(self.err("net section outside *D_NET"));
        }
        if key == "*NAME_MAP" && t.len() == 1 && self.spef.nets.is_empty() {
            self.section = Section::Names;
            return Ok(());
        }
        if key == "*PORTS" && t.len() == 1 && self.spef.nets.is_empty() {
            self.section = Section::Ports;
            return Ok(());
        }
        if self.section == Section::Names
            && key.starts_with('*')
            && key[1..].bytes().all(|b| b.is_ascii_digit())
        {
            if key.len() < 2 || t.len() != 2 {
                return Err(self.err("invalid name map record"));
            }
            if self.names.len() >= self.limits.max_nodes {
                return Err(self.budget("name map entry budget exceeded"));
            }
            let name = self.name(&t[1])?;
            if self.names.insert(key.into(), name).is_some() {
                return Err(self.err("duplicate name map id"));
            }
            return Ok(());
        }
        if self.section == Section::Ports && !key.starts_with('*')
            || self.section == Section::Ports
                && key
                    .get(1..)
                    .is_some_and(|s| s.chars().next().is_some_and(|c| c.is_ascii_digit()))
        {
            if t.len() < 2 || !["I", "O", "B"].contains(&t[1].as_str()) {
                return Err(self.err("invalid port record"));
            }
            let _ = self.name(key)?;
            return self.attributes(&t[2..]);
        }
        if self.section == Section::Ports && key == "*C" {
            return self.attributes(t);
        }
        if self.section == Section::End {
            return Err(self.err("header after detailed nets"));
        }
        if !self.header.insert(key.into()) {
            return Err(self.err("duplicate header field"));
        }
        match key {
            "*DESIGN_FLOW" => {
                for item in &t[1..] {
                    if !["NAME_SCOPE LOCAL", "PIN_CAP NONE"].contains(&item.as_str()) {
                        return Err(Error::new(
                            self.line,
                            "T0073",
                            format!("unsupported design flow option {item}"),
                        ));
                    }
                }
            }
            "*SPEF" | "*DATE" | "*VENDOR" | "*PROGRAM" | "*VERSION" => {
                if t.len() < 2 {
                    return Err(self.err("empty header field"));
                }
            }
            "*DESIGN" if t.len() == 2 => self.spef.design = t[1].clone(),
            "*DIVIDER" | "*DELIMITER" if t.len() == 2 && t[1].chars().count() == 1 => {
                let c = t[1].chars().next().unwrap();
                if !matches!(c, '/' | ':' | '.' | '|') {
                    return Err(self.err("unsupported divider/delimiter"));
                }
                if key == "*DIVIDER" {
                    self.spef.divider = c;
                } else {
                    self.spef.delimiter = c;
                }
            }
            "*BUS_DELIMITER" => {
                let s = t[1..].join("");
                if !["[]", "<>", "()", "{}"].contains(&s.as_str()) {
                    return Err(self.err("unsupported bus delimiters"));
                }
                let mut cs = s.chars();
                self.spef.bus_delimiter = (cs.next().unwrap(), cs.next().unwrap());
            }
            "*T_UNIT" | "*C_UNIT" | "*R_UNIT" | "*L_UNIT" if t.len() == 3 => {
                let mult = self.number(&t[1])?;
                if mult == 0.0 {
                    return Err(self.err("zero unit multiplier"));
                }
                let scale = match (key, t[2].as_str()) {
                    ("*T_UNIT", "NS") | ("*C_UNIT", "PF") | ("*R_UNIT", "KOHM") => 1.0,
                    ("*T_UNIT", "PS") | ("*C_UNIT", "FF") | ("*R_UNIT", "OHM") => 0.001,
                    ("*L_UNIT", "HENRY" | "MH" | "UH") => 1.0,
                    _ => return Err(self.err("unsupported unit")),
                };
                if !(mult * scale).is_finite() {
                    return Err(self.err("invalid unit scale"));
                }
                if key == "*C_UNIT" {
                    self.cap_scale = Some(mult * scale);
                }
                if key == "*R_UNIT" {
                    self.res_scale = Some(mult * scale);
                }
            }
            _ => {
                return Err(Error::new(
                    self.line,
                    "T0073",
                    format!("unsupported SPEF field {key}"),
                ));
            }
        }
        self.spef.metadata_records += 1;
        Ok(())
    }
    fn attributes(&mut self, t: &[String]) -> Result<(), Error> {
        let mut i = 0;
        while i < t.len() {
            match t[i].as_str() {
                "*C" if i + 2 < t.len() => {
                    // Physical coordinates may be negative; they do not enter RC timing.
                    for s in &t[i + 1..i + 3] {
                        if !s.parse::<f64>().is_ok_and(f64::is_finite) {
                            return Err(self.err("invalid coordinates"));
                        }
                    }
                    i += 3;
                }
                "*D" if i + 1 < t.len() => i += 2,
                _ => {
                    return Err(Error::new(
                        self.line,
                        "T0073",
                        format!("unsupported connection attribute {}", t[i]),
                    ));
                }
            }
            self.spef.metadata_records += 1;
        }
        Ok(())
    }
    fn net_line(&mut self, t: &[String]) -> Result<(), Error> {
        let key = t[0].as_str();
        match key {
            "*CONN" if t.len() == 1 && self.section == Section::End => {
                self.section = Section::Conn;
                return Ok(());
            }
            "*CAP" if t.len() == 1 && self.section == Section::Conn => {
                self.section = Section::Cap;
                return Ok(());
            }
            "*RES" if t.len() == 1 && matches!(self.section, Section::Conn | Section::Cap) => {
                self.section = Section::Res;
                return Ok(());
            }
            "*END" if t.len() == 1 && self.section != Section::End => {
                let mut net = self.current.take().unwrap();
                // Canonical reductions and node/edge order, independent of statement order.
                self.cap_terms
                    .sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
                for &(node, cap) in &self.cap_terms {
                    net.nodes[node as usize].cap_pf += cap;
                }
                let mut remap = vec![0u32; net.nodes.len()];
                net.nodes.sort_by(|a, b| a.name.cmp(&b.name));
                for (i, node) in net.nodes.iter().enumerate() {
                    remap[self.node_names[&node.name] as usize] = i as u32;
                }
                for c in &mut net.connections {
                    c.node = remap[c.node as usize];
                }
                net.connections.sort_by_key(|c| c.node);
                for r in &mut net.resistors {
                    let a = remap[r.a as usize];
                    let b = remap[r.b as usize];
                    r.a = a.min(b);
                    r.b = a.max(b);
                }
                net.resistors
                    .sort_by(|a, b| (a.a, a.b).cmp(&(b.a, b.b)).then(a.kohm.total_cmp(&b.kohm)));
                let sum: f64 = net.nodes.iter().map(|n| n.cap_pf).sum();
                if !sum.is_finite()
                    || (sum - net.total_cap_pf).abs() > 1e-8_f64.max(net.total_cap_pf * 1e-4)
                {
                    return Err(self.err(format!(
                        "net {} total capacitance mismatch: declared {}, records {} pF",
                        net.name, net.total_cap_pf, sum
                    )));
                }
                self.spef.nets.push(net);
                self.node_names.clear();
                self.conn_nodes.clear();
                self.cap_ids.clear();
                self.cap_terms.clear();
                self.res_ids.clear();
                self.section = Section::End;
                return Ok(());
            }
            _ => {}
        }
        if self.section == Section::Conn {
            if ["*C", "*D"].contains(&key) {
                return self.attributes(t);
            }
            if !["*I", "*P"].contains(&key)
                || t.len() < 3
                || !["I", "O", "B"].contains(&t[2].as_str())
            {
                return Err(self.err("invalid connection record"));
            }
            let name = self.name(&t[1])?;
            let node = self.node(name)?;
            if !self.conn_nodes.insert(node) {
                return Err(self.err("duplicate connection"));
            }
            self.current.as_mut().unwrap().connections.push(Connection {
                node,
                port: key == "*P",
                direction: t[2].chars().next().unwrap(),
            });
            return self.attributes(&t[3..]);
        }
        if !matches!(self.section, Section::Cap | Section::Res) {
            return Err(self.err("invalid net section order"));
        }
        self.element()?;
        let id: u64 = key.parse().map_err(|_| self.err("invalid RC element id"))?;
        let ids = if self.section == Section::Cap {
            &mut self.cap_ids
        } else {
            &mut self.res_ids
        };
        if !ids.insert(id) {
            return Err(self.err("duplicate RC element id"));
        }
        if self.section == Section::Cap {
            if t.len() != 3 && t.len() != 4 {
                return Err(self.err("invalid capacitance record"));
            }
            let cap = self.scaled(t.last().unwrap(), self.cap_scale.unwrap())?;
            let a = self.name(&t[1])?;
            let local = if t.len() == 4 {
                let b = self.name(&t[2])?;
                let net = self.current.as_ref().unwrap();
                let prefix = format!("{}{}", net.name, self.spef.delimiter);
                let is_local = |s: &str| {
                    self.node_names.contains_key(s) || s == net.name || s.starts_with(&prefix)
                };
                let local = match (is_local(&a), is_local(&b)) {
                    (true, false) => a,
                    (false, true) => b,
                    _ => return Err(self.err("coupling must have exactly one local endpoint")),
                };
                self.spef.coupling_records += 1;
                local
            } else {
                a
            };
            let node = self.node(local)?;
            self.cap_terms.push((node, cap));
        } else {
            if t.len() != 4 {
                return Err(self.err("invalid resistance record"));
            }
            let a = self.name(&t[1])?;
            let b = self.name(&t[2])?;
            let a = self.node(a)?;
            let b = self.node(b)?;
            let kohm = self.scaled(&t[3], self.res_scale.unwrap())?;
            self.current
                .as_mut()
                .unwrap()
                .resistors
                .push(Resistor { a, b, kohm });
        }
        Ok(())
    }
}
