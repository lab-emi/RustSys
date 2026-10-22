// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Fast streaming reader for single-module flat netlists (`RCLI-REQ-0019`).
//! Expression scratch is capped at MAX_RANGE_WIDTH *4 B, nesting at 64.
//! A C910 import keeps ~1.3 GiB source, <2 GiB frozen tables and bounded
//! name/connection indices; no whole-file tokens/AST or graph paths are built.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use rusteda_core::names::{check_verbatim, escape_segment};
use rusteda_core::netlist::{
    AssignRec, CONST0, CONST1, FrozenNetlist, InstRec, MAX_RANGE_WIDTH, NetRec, PinDir, PortDir,
    PortRec, SCALAR,
};
use rusteda_formats::verilog::number::{Bit4, Literal};
use rusteda_formats::verilog::token::KEYWORDS;

use super::{CellCatalog, CellInfo, range_width};

#[derive(Clone, Copy)]
enum SymSlice {
    Text(u32, u32),
    Esc(u32, u32),
}

struct SymStore {
    esc_buf: Vec<u8>,
}

impl SymStore {
    fn new() -> Self {
        Self {
            esc_buf: Vec::new(),
        }
    }
    fn verbatim(&self, s: usize, e: usize) -> Option<SymSlice> {
        Some(SymSlice::Text(
            u32::try_from(s).ok()?,
            u32::try_from(e).ok()?,
        ))
    }
    fn hierarchical(&mut self, text: &str, s: usize, e: usize) -> Option<SymSlice> {
        let raw = &text[s..e];
        if !raw.as_bytes().iter().any(|&b| b == b'\\' || b == b'/') {
            return self.verbatim(s, e);
        }
        let esc = escape_segment(raw);
        let start = u32::try_from(self.esc_buf.len()).ok()?;
        self.esc_buf.extend_from_slice(esc.as_bytes());
        Some(SymSlice::Esc(
            start,
            u32::try_from(self.esc_buf.len()).ok()?,
        ))
    }
    fn resolve<'a>(&'a self, text: &'a str, sym: SymSlice) -> &'a str {
        match sym {
            SymSlice::Text(s, e) => &text[s as usize..e as usize],
            SymSlice::Esc(s, e) => {
                std::str::from_utf8(&self.esc_buf[s as usize..e as usize]).unwrap_or("")
            }
        }
    }
}

struct StrTable<'a> {
    map: HashMap<&'a str, u32>,
    bytes: Vec<u8>,
    offs: Vec<u32>,
}

impl<'a> StrTable<'a> {
    fn with_capacity(cap: usize, byte_cap: usize) -> Self {
        let mut offs = Vec::with_capacity(cap + 1);
        offs.push(0);
        Self {
            map: HashMap::with_capacity(cap),
            bytes: Vec::with_capacity(byte_cap),
            offs,
        }
    }
    fn intern(&mut self, s: &'a str) -> u32 {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = (self.offs.len() - 1) as u32;
        self.bytes.extend_from_slice(s.as_bytes());
        self.offs.push(self.bytes.len() as u32);
        self.map.insert(s, id);
        id
    }
}

#[derive(Clone, Copy)]
enum FastDecl {
    Net {
        range: Option<(i32, i32)>,
        first_net: u32,
        is_port: bool,
        typed: bool,
    },
    Instance,
}

struct FastPort(SymSlice, u8, Option<(i32, i32)>);
struct FastNet(SymSlice, i32);
struct FastInst(SymSlice, u32);

struct Cur<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cur<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            bytes: text.as_bytes(),
            pos: 0,
        }
    }

    fn skip_ws(&mut self) -> Option<()> {
        let (b, n) = (self.bytes, self.bytes.len());
        while self.pos < n {
            let c = b[self.pos];
            if c.is_ascii_whitespace() {
                self.pos += 1;
            } else if c == b'/' && self.pos + 1 < n && b[self.pos + 1] == b'/' {
                self.pos += 2;
                while self.pos < n && b[self.pos] != b'\n' {
                    self.pos += 1;
                }
            } else if c == b'/' && self.pos + 1 < n && b[self.pos + 1] == b'*' {
                self.pos += 2;
                let rel = b[self.pos..].windows(2).position(|w| w == b"*/")?;
                self.pos += rel + 2;
            } else {
                break;
            }
        }
        Some(())
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws()?;
        self.bytes.get(self.pos).copied()
    }

    fn eat(&mut self, ch: u8) -> Option<bool> {
        self.skip_ws()?;
        if self.bytes.get(self.pos) == Some(&ch) {
            self.pos += 1;
            Some(true)
        } else {
            Some(false)
        }
    }

    fn kw(&mut self, kw: &[u8]) -> Option<bool> {
        self.skip_ws()?;
        let end = self.pos + kw.len();
        if end <= self.bytes.len() && &self.bytes[self.pos..end] == kw {
            let ok = !matches!(
                self.bytes.get(end),
                Some(c) if c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$'
            );
            if ok {
                self.pos = end;
                return Some(true);
            }
        }
        Some(false)
    }

    fn dir_kw(&mut self) -> Option<Option<PortDir>> {
        if self.kw(b"input")? {
            Some(Some(PortDir::Input))
        } else if self.kw(b"output")? {
            Some(Some(PortDir::Output))
        } else if self.kw(b"inout")? {
            Some(Some(PortDir::Inout))
        } else {
            Some(None)
        }
    }

    fn ident(&mut self) -> Option<(usize, usize)> {
        self.skip_ws()?;
        let (b, n) = (self.bytes, self.bytes.len());
        let c = *b.get(self.pos)?;
        if c == b'\\' {
            self.pos += 1;
            let start = self.pos;
            while self.pos < n && (33..=126).contains(&b[self.pos]) {
                self.pos += 1;
            }
            let end = self.pos;
            if end == start || (self.pos < n && !b[self.pos].is_ascii_whitespace()) {
                return None;
            }
            if self.pos < n {
                self.pos += 1;
            }
            Some((start, end))
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = self.pos;
            self.pos += 1;
            while self.pos < n
                && (b[self.pos].is_ascii_alphanumeric()
                    || b[self.pos] == b'_'
                    || b[self.pos] == b'$')
            {
                self.pos += 1;
            }
            let end = self.pos;
            if KEYWORDS.binary_search(&&self.text[start..end]).is_ok() {
                self.pos = start;
                return None;
            }
            Some((start, end))
        } else {
            None
        }
    }

    fn int_literal(&mut self) -> Option<i32> {
        self.skip_ws()?;
        let neg = self.eat(b'-')?;
        if neg {
            self.skip_ws()?;
        }
        let start = self.pos;
        let mut val: i64 = 0;
        while let Some(&c) = self.bytes.get(self.pos) {
            if c.is_ascii_digit() {
                val = val.checked_mul(10)?.checked_add(i64::from(c - b'0'))?;
                self.pos += 1;
            } else if c == b'_' && self.pos > start {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start
            || matches!(
                self.bytes.get(self.pos),
                Some(b'\'' | b'.' | b'a'..=b'z' | b'A'..=b'Z' | b'$')
            )
        {
            return None;
        }
        i32::try_from(if neg { -val } else { val })
            .ok()
            .filter(|&v| v != i32::MIN)
    }

    fn opt_range(&mut self) -> Option<Option<(i32, i32)>> {
        if !self.eat(b'[')? {
            return Some(None);
        }
        let msb = self.int_literal()?;
        if !self.eat(b':')? {
            return None;
        }
        let lsb = self.int_literal()?;
        if !self.eat(b']')? || range_width(msb, lsb) > u64::from(MAX_RANGE_WIDTH) {
            return None;
        }
        Some(Some((msb, lsb)))
    }
}

fn port_dir_code(dir: PortDir) -> u8 {
    match dir {
        PortDir::Input => 0,
        PortDir::Output => 1,
        PortDir::Inout => 2,
    }
}

fn pin_dir_code(dir: PinDir) -> u8 {
    match dir {
        PinDir::Input => 0,
        PinDir::Output => 1,
        PinDir::Inout => 2,
        PinDir::Other => 3,
    }
}

struct ExprReader<'a, 'b> {
    syms: &'b mut SymStore,
    nets: &'b mut Vec<FastNet>,
    decls: &'b mut HashMap<&'a str, FastDecl>,
    allow_implicit: bool,
}

impl<'a> ExprReader<'a, '_> {
    /// Append bits in Verilog MSB-to-LSB order, as used by frozen bus tables.
    fn append(&mut self, cur: &mut Cur<'a>, out: &mut Vec<u32>, depth: usize) -> Option<()> {
        if depth >= 64 || out.len() >= MAX_RANGE_WIDTH as usize {
            return None;
        }
        if cur.eat(b'{')? {
            loop {
                self.append(cur, out, depth + 1)?;
                if cur.eat(b'}')? {
                    return Some(());
                }
                if !cur.eat(b',')? {
                    return None;
                }
            }
        }
        if cur.peek()?.is_ascii_digit() {
            let mut width = 0u32;
            while let Some(&ch) = cur.bytes.get(cur.pos) {
                if ch.is_ascii_digit() {
                    width = width.checked_mul(10)?.checked_add(u32::from(ch - b'0'))?;
                } else if ch != b'_' {
                    break;
                }
                cur.pos += 1;
            }
            if width == 0 || width > MAX_RANGE_WIDTH || !cur.eat(b'\'')? {
                return None;
            }
            let base = cur.bytes.get(cur.pos)?.to_ascii_lowercase();
            // Signed/unknown expressions use the diagnostic AST path. Decimal
            // digits are bounded separately because conversion is quadratic.
            if !matches!(base, b'b' | b'o' | b'h' | b'd') {
                return None;
            }
            cur.pos += 1;
            let start = cur.pos;
            while let Some(ch) = cur.bytes.get(cur.pos) {
                if !(ch.is_ascii_hexdigit() || *ch == b'_') {
                    break;
                }
                cur.pos += 1;
                if cur.pos - start > MAX_RANGE_WIDTH as usize {
                    return None;
                }
            }
            if base == b'd' && cur.pos - start > 32 {
                return None;
            }
            if cur.pos == start || out.len().checked_add(width as usize)? > MAX_RANGE_WIDTH as usize
            {
                return None;
            }
            let literal = Literal {
                size: Some(width),
                plain: false,
                signed: false,
                base: char::from(base),
                digits: cur.text[start..cur.pos]
                    .chars()
                    .filter(|&ch| ch != '_')
                    .collect(),
            };
            for bit in literal.bits().ok()?.into_iter().rev() {
                out.push(match bit {
                    Bit4::Zero => CONST0,
                    Bit4::One => CONST1,
                    _ => return None,
                });
            }
            return Some(());
        }
        let (start, end) = cur.ident()?;
        let name = &cur.text[start..end];
        let selected = if cur.eat(b'[')? {
            let first = cur.int_literal()?;
            let last = if cur.eat(b':')? {
                cur.int_literal()?
            } else {
                first
            };
            if !cur.eat(b']')? {
                return None;
            }
            Some((first, last))
        } else {
            None
        };
        match self.decls.get(name).copied() {
            Some(FastDecl::Net {
                range: Some((msb, lsb)),
                first_net,
                ..
            }) => {
                let (first, last) = selected.unwrap_or((msb, lsb));
                let width = range_width(first, last);
                if first < msb.min(lsb)
                    || first > msb.max(lsb)
                    || last < msb.min(lsb)
                    || last > msb.max(lsb)
                    || (first != last && (first > last) != (msb > lsb))
                    || width > u64::from(MAX_RANGE_WIDTH)
                    || out.len().checked_add(width as usize)? > MAX_RANGE_WIDTH as usize
                {
                    return None;
                }
                for k in 0..width {
                    let index =
                        i64::from(first) + if first > last { -(k as i64) } else { k as i64 };
                    let offset = (i64::from(msb) - index).unsigned_abs();
                    out.push(first_net.checked_add(u32::try_from(offset).ok()?)?);
                }
                Some(())
            }
            Some(FastDecl::Net {
                range: None,
                first_net,
                ..
            }) if selected.is_none() => {
                out.push(first_net);
                Some(())
            }
            None if self.allow_implicit && selected.is_none() => {
                let symbol = self.syms.hierarchical(cur.text, start, end)?;
                let first_net = u32::try_from(self.nets.len()).ok()?;
                self.nets.push(FastNet(symbol, SCALAR));
                self.decls.insert(
                    name,
                    FastDecl::Net {
                        range: None,
                        first_net,
                        is_port: false,
                        typed: true,
                    },
                );
                out.push(first_net);
                Some(())
            }
            _ => None,
        }
    }
}

#[allow(clippy::too_many_lines)]
pub(super) fn try_fast_flat<'a>(
    text: &'a str,
    catalog: &'a CellCatalog,
) -> Option<FrozenNetlist<'static>> {
    if text.as_bytes().contains(&b'`') || text.as_bytes().windows(2).any(|w| w == b"(*") {
        return None;
    }
    let mut cur = Cur::new(text);
    if !cur.kw(b"module")? {
        return None;
    }
    let (mod_s, mod_e) = cur.ident()?;
    let mod_name = &text[mod_s..mod_e];
    check_verbatim(mod_name).ok()?;

    let mut syms = SymStore::new();
    let mod_sym = syms.verbatim(mod_s, mod_e)?;
    let mut budget = (text.len() as u64).max(u64::from(MAX_RANGE_WIDTH));
    let mut decls: HashMap<&'a str, FastDecl> = HashMap::new();
    let mut ports: Vec<FastPort> = Vec::new();
    let mut port_bit_start: Vec<u32> = vec![0];
    let mut port_bit_net: Vec<u32> = Vec::new();
    let mut nets: Vec<FastNet> = Vec::new();

    let push_nets = |syms: &mut SymStore,
                     nets: &mut Vec<FastNet>,
                     budget: &mut u64,
                     s: usize,
                     e: usize,
                     range: Option<(i32, i32)>|
     -> Option<(SymSlice, u32, u32)> {
        let sym = syms.hierarchical(text, s, e)?;
        let first = u32::try_from(nets.len()).ok()?;
        if let Some((msb, lsb)) = range {
            let w = range_width(msb, lsb);
            *budget = budget.checked_sub(w)?;
            if msb >= lsb {
                for idx in (lsb..=msb).rev() {
                    nets.push(FastNet(sym, idx));
                }
            } else {
                for idx in msb..=lsb {
                    nets.push(FastNet(sym, idx));
                }
            }
            Some((sym, first, w as u32))
        } else {
            nets.push(FastNet(sym, SCALAR));
            Some((sym, first, 1))
        }
    };

    if !cur.eat(b'(')? {
        return None;
    }
    let mut non_ansi_hdr: Vec<(usize, usize)> = Vec::new();
    let is_non_ansi;
    if cur.eat(b')')? {
        is_non_ansi = false;
    } else {
        let save = cur.pos;
        let has_dir = cur.dir_kw()?.is_some();
        cur.pos = save;
        if has_dir {
            is_non_ansi = false;
            let (mut cdir, mut crange) = (PortDir::Input, None);
            loop {
                if let Some(d) = cur.dir_kw()? {
                    cdir = d;
                    let _ = cur.kw(b"wire")?;
                    if cur.kw(b"logic")? || cur.kw(b"reg")? || cur.kw(b"signed")? {
                        return None;
                    }
                    crange = cur.opt_range()?;
                }
                let (ps, pe) = cur.ident()?;
                let pname = &text[ps..pe];
                if decls.contains_key(pname) {
                    return None;
                }
                let (sym, first_net, len) =
                    push_nets(&mut syms, &mut nets, &mut budget, ps, pe, crange)?;
                ports.push(FastPort(sym, port_dir_code(cdir), crange));
                port_bit_net.extend(first_net..first_net + len);
                port_bit_start.push(port_bit_net.len() as u32);
                decls.insert(
                    pname,
                    FastDecl::Net {
                        range: crange,
                        first_net,
                        is_port: true,
                        typed: true,
                    },
                );
                if cur.eat(b',')? {
                    continue;
                }
                if cur.eat(b')')? {
                    break;
                }
                return None;
            }
        } else {
            is_non_ansi = true;
            let mut seen = HashSet::new();
            loop {
                let (ps, pe) = cur.ident()?;
                if !seen.insert(&text[ps..pe]) {
                    return None;
                }
                non_ansi_hdr.push((ps, pe));
                if cur.eat(b',')? {
                    continue;
                }
                if cur.eat(b')')? {
                    break;
                }
                return None;
            }
        }
    }
    if !cur.eat(b';')? {
        return None;
    }

    if is_non_ansi {
        let hdr_set: HashSet<&str> = non_ansi_hdr.iter().map(|&(s, e)| &text[s..e]).collect();
        let mut pdecls: HashMap<&str, (PortDir, Option<(i32, i32)>, bool)> =
            HashMap::with_capacity(non_ansi_hdr.len());
        let mut wredecls: HashMap<&str, Option<(i32, i32)>> = HashMap::new();
        loop {
            let save = cur.pos;
            if let Some(d) = cur.dir_kw()? {
                let typed = cur.kw(b"wire")?;
                if cur.kw(b"logic")? || cur.kw(b"reg")? || cur.kw(b"signed")? {
                    return None;
                }
                let range = cur.opt_range()?;
                loop {
                    let (ps, pe) = cur.ident()?;
                    let pname = &text[ps..pe];
                    if !hdr_set.contains(pname) || pdecls.insert(pname, (d, range, typed)).is_some()
                    {
                        return None;
                    }
                    if cur.eat(b',')? {
                        continue;
                    }
                    if cur.eat(b';')? {
                        break;
                    }
                    return None;
                }
                continue;
            }
            if cur.kw(b"wire")? {
                if cur.kw(b"signed")? || cur.kw(b"logic")? {
                    return None;
                }
                let range = cur.opt_range()?;
                let (ws, we) = cur.ident()?;
                let wname = &text[ws..we];
                if hdr_set.contains(wname) && cur.eat(b';')? {
                    if wredecls.insert(wname, range).is_some() {
                        return None;
                    }
                    continue;
                }
            }
            cur.pos = save;
            break;
        }
        if pdecls.len() != non_ansi_hdr.len() {
            return None;
        }
        for &(ps, pe) in &non_ansi_hdr {
            let pname = &text[ps..pe];
            let &(dir, range, mut typed) = pdecls.get(pname)?;
            if let Some(&wr) = wredecls.get(pname) {
                if typed || wr != range {
                    return None;
                }
                typed = true;
            }
            let (sym, first_net, len) =
                push_nets(&mut syms, &mut nets, &mut budget, ps, pe, range)?;
            ports.push(FastPort(sym, port_dir_code(dir), range));
            port_bit_net.extend(first_net..first_net + len);
            port_bit_start.push(port_bit_net.len() as u32);
            decls.insert(
                pname,
                FastDecl::Net {
                    range,
                    first_net,
                    is_port: true,
                    typed,
                },
            );
        }
    }

    let mut used_cells: Vec<(SymSlice, &'a CellInfo)> = Vec::new();
    let mut cell_map: HashMap<&'a str, u32> = HashMap::new();
    let mut instances: Vec<FastInst> = Vec::new();
    let mut inst_pin_start: Vec<u32> = vec![0];
    let mut pin_cellpin: Vec<u32> = Vec::new();
    let mut pin_net: Vec<u32> = Vec::new();
    let mut assigns: Vec<AssignRec> = Vec::new();
    let mut seen_body = false;

    let resolve_expr = |cur: &mut Cur<'a>,
                        syms: &mut SymStore,
                        nets: &mut Vec<FastNet>,
                        decls: &mut HashMap<&'a str, FastDecl>,
                        allow_implicit: bool,
                        out: &mut Vec<u32>|
     -> Option<()> {
        out.clear();
        ExprReader {
            syms,
            nets,
            decls,
            allow_implicit,
        }
        .append(cur, out, 0)
    };

    let (mut lhs_buf, mut rhs_buf, mut conn_pins) = (Vec::new(), Vec::new(), Vec::new());
    loop {
        if cur.kw(b"endmodule")? {
            break;
        }
        if cur.kw(b"wire")? {
            if seen_body || cur.kw(b"signed")? || cur.kw(b"logic")? {
                return None;
            }
            let range = cur.opt_range()?;
            loop {
                let (ws, we) = cur.ident()?;
                let wname = &text[ws..we];
                if let Some(ex) = decls.get_mut(wname) {
                    match ex {
                        FastDecl::Net {
                            range: pr,
                            is_port: true,
                            typed,
                            ..
                        } if is_non_ansi && !*typed && *pr == range => {
                            *typed = true;
                        }
                        _ => return None,
                    }
                } else {
                    let (_, first_net, _) =
                        push_nets(&mut syms, &mut nets, &mut budget, ws, we, range)?;
                    decls.insert(
                        wname,
                        FastDecl::Net {
                            range,
                            first_net,
                            is_port: false,
                            typed: true,
                        },
                    );
                }
                if cur.eat(b',')? {
                    continue;
                }
                if cur.eat(b';')? {
                    break;
                }
                return None;
            }
            continue;
        }
        if cur.kw(b"assign")? {
            seen_body = true;
            loop {
                resolve_expr(
                    &mut cur,
                    &mut syms,
                    &mut nets,
                    &mut decls,
                    true,
                    &mut lhs_buf,
                )?;
                if !cur.eat(b'=')? {
                    return None;
                }
                resolve_expr(
                    &mut cur,
                    &mut syms,
                    &mut nets,
                    &mut decls,
                    false,
                    &mut rhs_buf,
                )?;
                if lhs_buf.len() != rhs_buf.len() {
                    return None;
                }
                for (&l, &r) in lhs_buf.iter().zip(rhs_buf.iter()) {
                    if l == CONST0 || l == CONST1 {
                        return None;
                    }
                    assigns.push(AssignRec { lhs: l, rhs: r });
                }
                if cur.eat(b',')? {
                    continue;
                }
                if cur.eat(b';')? {
                    break;
                }
                return None;
            }
            continue;
        }

        seen_body = true;
        let (cs, ce) = cur.ident()?;
        let cname = &text[cs..ce];
        if cname == mod_name {
            return None;
        }
        let cinfo = catalog.cells.get(cname)?;
        if !cinfo.conflicts.is_empty() {
            return None;
        }
        let cell_idx = if let Some(&idx) = cell_map.get(cname) {
            idx
        } else {
            let idx = u32::try_from(used_cells.len()).ok()?;
            used_cells.push((syms.verbatim(cs, ce)?, cinfo));
            cell_map.insert(cname, idx);
            idx
        };

        loop {
            let (is, ie) = cur.ident()?;
            let iname = &text[is..ie];
            if decls.insert(iname, FastDecl::Instance).is_some() {
                return None;
            }
            instances.push(FastInst(syms.hierarchical(text, is, ie)?, cell_idx));
            if !cur.eat(b'(')? {
                return None;
            }
            conn_pins.clear();
            if !cur.eat(b')')? {
                loop {
                    if !cur.eat(b'.')? {
                        return None;
                    }
                    let (ps, pe) = cur.ident()?;
                    let &pin_idx = cinfo.pin_index.get(&text[ps..pe])?;
                    if conn_pins.contains(&pin_idx) {
                        return None;
                    }
                    conn_pins.push(pin_idx);
                    if !cur.eat(b'(')? {
                        return None;
                    }
                    if !cur.eat(b')')? {
                        resolve_expr(
                            &mut cur,
                            &mut syms,
                            &mut nets,
                            &mut decls,
                            true,
                            &mut rhs_buf,
                        )?;
                        if rhs_buf.len() != 1 || !cur.eat(b')')? {
                            return None;
                        }
                        pin_cellpin.push(pin_idx);
                        pin_net.push(rhs_buf[0]);
                    }
                    if cur.eat(b',')? {
                        continue;
                    }
                    if cur.eat(b')')? {
                        break;
                    }
                    return None;
                }
            }
            inst_pin_start.push(u32::try_from(pin_net.len()).ok()?);
            if cur.eat(b',')? {
                continue;
            }
            if cur.eat(b';')? {
                break;
            }
            return None;
        }
    }
    if cur.peek().is_some() {
        return None;
    }
    drop(decls);
    drop(cell_map);

    let est = 1 + used_cells.len() * 4 + ports.len() + nets.len() + instances.len();
    let mut st = StrTable::with_capacity(est, est * 10);
    let module_name = st.intern(syms.resolve(text, mod_sym));

    let mut cells = Vec::with_capacity(used_cells.len());
    let mut cell_pin_start = Vec::with_capacity(used_cells.len() + 1);
    cell_pin_start.push(0);
    let (mut cell_pin_name, mut cell_pin_dir) = (Vec::new(), Vec::new());
    for &(csym, info) in &used_cells {
        cells.push(st.intern(syms.resolve(text, csym)));
        for (pname, pdir) in &info.pins {
            cell_pin_name.push(st.intern(pname.as_str()));
            cell_pin_dir.push(pin_dir_code(*pdir));
        }
        cell_pin_start.push(cell_pin_name.len() as u32);
    }

    let port_recs: Vec<PortRec> = ports
        .iter()
        .map(|FastPort(name, dir, range)| {
            let (msb, lsb) = range.unwrap_or((0, 0));
            PortRec {
                name: st.intern(syms.resolve(text, *name)),
                dir: *dir,
                is_bus: u8::from(range.is_some()),
                _pad: 0,
                msb,
                lsb,
            }
        })
        .collect();
    drop(ports);

    let net_recs: Vec<NetRec> = nets
        .iter()
        .map(|FastNet(name, index)| NetRec {
            name: st.intern(syms.resolve(text, *name)),
            index: *index,
        })
        .collect();
    let n_nets = net_recs.len();
    drop(nets);

    let inst_recs: Vec<InstRec> = instances
        .iter()
        .map(|FastInst(name, cell)| InstRec {
            name: st.intern(syms.resolve(text, *name)),
            cell: *cell,
        })
        .collect();
    drop(instances);

    let StrTable {
        bytes: str_bytes,
        offs: str_offs,
        map,
    } = st;
    drop(map);
    drop(syms);

    let mut net_pin_start = vec![0u32; n_nets + 1];
    for &v in &pin_net {
        if (v as usize) < n_nets {
            net_pin_start[v as usize + 1] += 1;
        }
    }
    for k in 0..n_nets {
        net_pin_start[k + 1] += net_pin_start[k];
    }
    let mut fill = net_pin_start.clone();
    let mut net_pins = vec![0u32; net_pin_start[n_nets] as usize];
    for (p, &v) in pin_net.iter().enumerate() {
        if (v as usize) < n_nets {
            net_pins[fill[v as usize] as usize] = p as u32;
            fill[v as usize] += 1;
        }
    }
    drop(fill);

    let module = vec![
        module_name,
        port_recs.len() as u32,
        n_nets as u32,
        inst_recs.len() as u32,
        pin_net.len() as u32,
        assigns.len() as u32,
    ];
    Some(FrozenNetlist {
        str_bytes: Cow::Owned(str_bytes),
        str_offs: Cow::Owned(str_offs),
        module: Cow::Owned(module),
        cells: Cow::Owned(cells),
        cell_pin_start: Cow::Owned(cell_pin_start),
        cell_pin_name: Cow::Owned(cell_pin_name),
        cell_pin_dir: Cow::Owned(cell_pin_dir),
        ports: Cow::Owned(port_recs),
        port_bit_start: Cow::Owned(port_bit_start),
        port_bit_net: Cow::Owned(port_bit_net),
        nets: Cow::Owned(net_recs),
        instances: Cow::Owned(inst_recs),
        inst_pin_start: Cow::Owned(inst_pin_start),
        pin_cellpin: Cow::Owned(pin_cellpin),
        pin_net: Cow::Owned(pin_net),
        assigns: Cow::Owned(assigns),
        net_pin_start: Cow::Owned(net_pin_start),
        net_pins: Cow::Owned(net_pins),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_vectors_preserve_bit_order() {
        for (text, first, expected) in [
            (
                "module top(input [7:0] a, output [7:0] y); assign y = {a[3:0], a[7:4]}; endmodule",
                8,
                vec![4, 5, 6, 7, 0, 1, 2, 3],
            ),
            (
                "module top(input [0:7] a, output [7:0] y); assign y = {a[4:7], a[0:3]}; endmodule",
                8,
                vec![4, 5, 6, 7, 0, 1, 2, 3],
            ),
            (
                "module top(input [7:0] a, output [7:0] y); assign {y[7:4], y[3:0]} = {2'h2, {a[3:2], 4'b0101}}; endmodule",
                8,
                vec![CONST1, CONST0, 4, 5, CONST0, CONST1, CONST0, CONST1],
            ),
            (
                "module top(output [3:0] y); assign y = {1'd1, 3'o2}; endmodule",
                0,
                vec![CONST1, CONST0, CONST1, CONST0],
            ),
        ] {
            let catalog = CellCatalog::default();
            let fast = try_fast_flat(text, &catalog).expect("streamed vector import");
            fast.validate().unwrap();
            assert_eq!(
                fast.assigns.iter().map(|a| a.lhs).collect::<Vec<_>>(),
                (first..first + expected.len() as u32).collect::<Vec<_>>()
            );
            assert_eq!(
                fast.assigns.iter().map(|a| a.rhs).collect::<Vec<_>>(),
                expected,
                "{text}"
            );
        }
    }

    #[test]
    fn streaming_expression_limits_and_unknowns_fall_back() {
        let catalog = CellCatalog::default();
        for rhs in ["a[0:3]", "4'bx001", "4'b000z", "4'b0001 | a", "1048577'b0"] {
            let text =
                format!("module top(input [3:0] a, output [3:0] y); assign y = {rhs}; endmodule");
            assert!(try_fast_flat(&text, &catalog).is_none(), "{rhs}");
        }
        let text = format!(
            "module top(input a, output y); assign y = {}a{}; endmodule",
            "{".repeat(65),
            "}".repeat(65)
        );
        assert!(try_fast_flat(&text, &catalog).is_none());
    }
}
