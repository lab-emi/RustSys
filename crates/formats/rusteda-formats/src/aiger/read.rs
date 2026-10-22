// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The AIGER parser (AIGER-REQ-0010 to AIGER-REQ-0032): header, sections, binary AND gates,
//! symbols and comments, with every allocation bounded by the header limits and by the data that
//! follows the header.

use super::{
    Aig, BINARY, CYCLE, DEFINITION, Diag, HEADER, LIMIT, LINE, Latch, Options, PROPERTIES, Reset,
    SYMBOLS, Symbols, UNDEFINED,
};

/// A cursor over the bytes.
struct Cur<'a> {
    data: &'a [u8],
    pos: usize,
    line: u32,
    name: &'a str,
}

impl<'a> Cur<'a> {
    /// The next line without its line feed and a carriage return before it (AIGER-REQ-0020).
    fn next_line(&mut self) -> Option<(&'a [u8], u32)> {
        if self.pos >= self.data.len() {
            return None;
        }
        let rest = &self.data[self.pos..];
        let (l, adv) = match rest.iter().position(|&b| b == b'\n') {
            Some(i) => (&rest[..i], i + 1),
            None => (rest, rest.len()),
        };
        self.pos += adv;
        self.line += 1;
        Some((l.strip_suffix(b"\r").unwrap_or(l), self.line))
    }

    fn err(&self, line: u32, code: &str, msg: impl Into<String>) -> Diag {
        Diag::error(code, msg).at(self.name, line.max(1), 1)
    }

    /// The next line, which must exist.
    fn need(&mut self, what: &str) -> Result<(&'a [u8], u32), Diag> {
        let at = self.line + 1;
        self.next_line()
            .ok_or_else(|| self.err(at, LINE, format!("the data ends before {what}")))
    }

    /// The numbers of the next line (AIGER-REQ-0012): `counts` lists the accepted counts.
    fn numbers(&mut self, what: &str, counts: &[usize]) -> Result<(Vec<u32>, u32), Diag> {
        let (l, at) = self.need(what)?;
        let ns: Option<Vec<u32>> = l.split(|&b| b == b' ').map(num).collect();
        match ns {
            Some(v) if counts.contains(&v.len()) => Ok((v, at)),
            _ => Err(self.err(
                at,
                LINE,
                format!(
                    "{what}: expected {} decimal numbers separated by single spaces, found '{}'",
                    counts
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" or "),
                    String::from_utf8_lossy(&l[..l.len().min(80)])
                ),
            )),
        }
    }
}

/// A decimal number that fits in 32 bits, without sign or leading zeros (AIGER-REQ-0010).
fn num(w: &[u8]) -> Option<u32> {
    if w.is_empty() || w.len() > 10 || !w.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if w.len() > 1 && w[0] == b'0' {
        return None;
    }
    let v = w.iter().fold(0u64, |a, &b| a * 10 + u64::from(b - b'0'));
    u32::try_from(v).ok()
}

/// What defines a variable.
const NONE: u8 = 0;
const INPUT: u8 = 1;
const LATCH: u8 = 2;
const AND: u8 = 3;

/// The header numbers.
struct Header {
    binary: bool,
    m: u32,
    i: u32,
    l: u32,
    o: u32,
    a: u32,
    b: u32,
    c: u32,
    j: u32,
    f: u32,
}

fn header(cur: &mut Cur<'_>, opts: &Options) -> Result<Header, Diag> {
    let Some((h, at)) = cur.next_line() else {
        return Err(cur.err(1, HEADER, "empty file: no AIGER header"));
    };
    let words: Vec<&[u8]> = h.split(|&b| b == b' ').collect();
    let binary = match words[0] {
        b"aag" => false,
        b"aig" => true,
        _ => {
            return Err(cur.err(at, HEADER, "the header does not start with 'aag' or 'aig'"));
        }
    };
    let ns: Option<Vec<u32>> = words[1..].iter().map(|w| num(w)).collect();
    let ns = match ns {
        Some(v) if (5..=9).contains(&v.len()) => v,
        _ => {
            return Err(cur.err(
                at,
                HEADER,
                "the header holds 5 to 9 decimal numbers M I L O A [B C J F] separated by single \
                 spaces",
            ));
        }
    };
    let g = |k: usize| ns.get(k).copied().unwrap_or(0);
    let hd = Header {
        binary,
        m: g(0),
        i: g(1),
        l: g(2),
        o: g(3),
        a: g(4),
        b: g(5),
        c: g(6),
        j: g(7),
        f: g(8),
    };
    let defs = u64::from(hd.i) + u64::from(hd.l) + u64::from(hd.a);
    if u64::from(hd.m) < defs || (binary && u64::from(hd.m) != defs) {
        return Err(cur.err(
            at,
            HEADER,
            format!(
                "M = {} must be {} I + L + A = {defs}",
                hd.m,
                if binary { "equal to" } else { "at least" }
            ),
        ));
    }
    if hd.m > opts.limits.max_vars {
        return Err(cur.err(
            at,
            LIMIT,
            format!(
                "M = {} exceeds the limit of {} variables",
                hd.m, opts.limits.max_vars
            ),
        ));
    }
    let items = [hd.o, hd.b, hd.c, hd.j, hd.f]
        .iter()
        .map(|&x| u64::from(x))
        .sum::<u64>();
    if items > u64::from(opts.limits.max_items) {
        return Err(cur.err(
            at,
            LIMIT,
            format!(
                "{items} outputs and properties exceed the limit of {}",
                opts.limits.max_items
            ),
        ));
    }
    if !opts.allow_properties && hd.b + hd.c + hd.j + hd.f > 0 {
        return Err(cur.err(
            at,
            PROPERTIES,
            "bad-state, constraint, justice and fairness sections are properties for model \
             checking, not synthesizable outputs",
        ));
    }
    // Every line takes at least two bytes, every binary AND gate two (AIGER-REQ-0030).
    let lines = u64::from(hd.l)
        + items
        + if binary {
            0
        } else {
            u64::from(hd.i) + u64::from(hd.a)
        };
    let need = 2 * lines + if binary { 2 * u64::from(hd.a) } else { 0 };
    let have = (cur.data.len() - cur.pos) as u64;
    if need > have + 1 {
        return Err(cur.err(
            at,
            LIMIT,
            format!("the header announces at least {need} bytes of data, but {have} follow"),
        ));
    }
    Ok(hd)
}

/// Parses an AIGER file (AIGER-REQ-0001 to AIGER-REQ-0032).
pub(super) fn read(data: &[u8], name: &str, opts: &Options) -> Result<Aig, Diag> {
    let mut cur = Cur {
        data,
        pos: 0,
        line: 0,
        name,
    };
    let hd = header(&mut cur, opts)?;
    let m = hd.m;
    let max_lit = 2 * u64::from(m) + 1;
    let mut defined = vec![NONE; m as usize + 1];
    // Reads defining literal `lit` of kind `kind` (AIGER-REQ-0011).
    let define = |cur: &Cur<'_>, defined: &mut [u8], lit: u32, kind: u8, at: u32| {
        if lit < 2 || lit & 1 == 1 || u64::from(lit) > max_lit {
            return Err(cur.err(
                at,
                DEFINITION,
                format!(
                    "{lit} is not an even literal from 2 to 2M = {}",
                    2 * u64::from(m)
                ),
            ));
        }
        let v = (lit >> 1) as usize;
        if defined[v] != NONE {
            return Err(cur.err(at, DEFINITION, format!("variable {v} is defined twice")));
        }
        defined[v] = kind;
        Ok(())
    };
    let bound = |cur: &Cur<'_>, lit: u32, at: u32| {
        if u64::from(lit) > max_lit {
            return Err(cur.err(
                at,
                DEFINITION,
                format!("literal {lit} is above 2M + 1 = {max_lit}"),
            ));
        }
        Ok(lit)
    };
    let mut g = Aig {
        max_var: m,
        binary: hd.binary,
        ..Aig::default()
    };
    // References to check once every variable is defined: literal and line.
    let mut refs: Vec<(u32, u32)> = Vec::new();
    g.inputs.reserve(hd.i as usize);
    for k in 0..hd.i {
        if hd.binary {
            let lit = 2 * (k + 1);
            defined[(k + 1) as usize] = INPUT;
            g.inputs.push(lit);
        } else {
            let (v, at) = cur.numbers("an input line", &[1])?;
            define(&cur, &mut defined, v[0], INPUT, at)?;
            g.inputs.push(v[0]);
        }
    }
    g.latches.reserve(hd.l as usize);
    for k in 0..hd.l {
        let (v, at) = if hd.binary {
            let (v, at) = cur.numbers("a latch line", &[1, 2])?;
            let mut w = vec![2 * (hd.i + k + 1)];
            w.extend(v);
            (w, at)
        } else {
            cur.numbers("a latch line", &[2, 3])?
        };
        let lit = v[0];
        if hd.binary {
            defined[(lit >> 1) as usize] = LATCH;
        } else {
            define(&cur, &mut defined, lit, LATCH, at)?;
        }
        let next = bound(&cur, v[1], at)?;
        refs.push((next, at));
        let reset = match v.get(2).copied() {
            None | Some(0) => Reset::Zero,
            Some(1) => Reset::One,
            Some(r) if r == lit => Reset::Uninitialized,
            Some(r) => {
                return Err(cur.err(
                    at,
                    DEFINITION,
                    format!("latch reset {r} is neither 0, 1 nor the latch literal {lit}"),
                ));
            }
        };
        g.latches.push(Latch { lit, next, reset });
    }
    let single =
        |cur: &mut Cur<'_>, n: u32, what: &str, out: &mut Vec<u32>, refs: &mut Vec<(u32, u32)>| {
            out.reserve(n as usize);
            for _ in 0..n {
                let (v, at) = cur.numbers(what, &[1])?;
                let lit = bound(cur, v[0], at)?;
                refs.push((lit, at));
                out.push(lit);
            }
            Ok::<(), Diag>(())
        };
    single(&mut cur, hd.o, "an output line", &mut g.outputs, &mut refs)?;
    single(&mut cur, hd.b, "a bad-state line", &mut g.bad, &mut refs)?;
    single(
        &mut cur,
        hd.c,
        "a constraint line",
        &mut g.constraints,
        &mut refs,
    )?;
    let mut sizes = Vec::with_capacity(hd.j as usize);
    for _ in 0..hd.j {
        let (v, _) = cur.numbers("a justice size line", &[1])?;
        sizes.push(v[0]);
    }
    let total: u64 = sizes.iter().map(|&s| u64::from(s)).sum();
    if 2 * total > (data.len() - cur.pos) as u64 + 1 {
        return Err(cur.err(
            cur.line,
            LIMIT,
            format!("the justice properties announce {total} literals, more than the data holds"),
        ));
    }
    for s in sizes {
        let mut lits = Vec::new();
        single(&mut cur, s, "a justice literal line", &mut lits, &mut refs)?;
        g.justice.push(lits);
    }
    single(
        &mut cur,
        hd.f,
        "a fairness line",
        &mut g.fairness,
        &mut refs,
    )?;
    let ands_at = cur.line + 1;
    g.ands.reserve(hd.a as usize);
    if hd.binary {
        binary_ands(&mut cur, &hd, &mut g.ands, &mut defined, ands_at)?;
    } else {
        for _ in 0..hd.a {
            let (v, at) = cur.numbers("an AND gate line", &[3])?;
            define(&cur, &mut defined, v[0], AND, at)?;
            let r0 = bound(&cur, v[1], at)?;
            let r1 = bound(&cur, v[2], at)?;
            refs.push((r0, at));
            refs.push((r1, at));
            g.ands.push([v[0], r0, r1]);
        }
    }
    for (lit, at) in refs {
        let v = (lit >> 1) as usize;
        if v != 0 && defined[v] == NONE {
            return Err(cur.err(
                at,
                UNDEFINED,
                format!("literal {lit} refers to variable {v}, which nothing defines"),
            ));
        }
    }
    if !hd.binary {
        g.ands = order(&cur, std::mem::take(&mut g.ands), m, ands_at)?;
    }
    g.symbols = symbols(&mut cur, &hd)?;
    Ok(g)
}

/// The binary AND gates (AIGER-REQ-0013).
fn binary_ands(
    cur: &mut Cur<'_>,
    hd: &Header,
    out: &mut Vec<[u32; 3]>,
    defined: &mut [u8],
    at: u32,
) -> Result<(), Diag> {
    for k in 0..hd.a {
        let lhs = 2 * (hd.i + hd.l + k + 1);
        let start = cur.pos;
        let mut delta = [0u32; 2];
        for d in &mut delta {
            *d = varint(cur).ok_or_else(|| {
                cur.err(
                    at,
                    BINARY,
                    format!(
                        "AND gate {k} at byte {start}: the data ends or a difference does not \
                         fit in 32 bits"
                    ),
                )
            })?;
        }
        if delta[0] == 0 || delta[0] > lhs || delta[1] > lhs - delta[0] {
            return Err(cur.err(
                at,
                BINARY,
                format!(
                    "AND gate {k} at byte {start}: the differences {} and {} do not give inputs \
                     below its left-hand side {lhs}",
                    delta[0], delta[1]
                ),
            ));
        }
        let r0 = lhs - delta[0];
        let r1 = r0 - delta[1];
        defined[(lhs >> 1) as usize] = AND;
        out.push([lhs, r0, r1]);
    }
    Ok(())
}

/// An unsigned number in 7-bit groups, least significant first, of at most five bytes.
fn varint(cur: &mut Cur<'_>) -> Option<u32> {
    let mut x = 0u64;
    for k in 0..5 {
        let b = *cur.data.get(cur.pos)?;
        cur.pos += 1;
        x |= u64::from(b & 0x7f) << (7 * k);
        if b & 0x80 == 0 {
            return u32::try_from(x).ok();
        }
    }
    None
}

/// The ASCII AND gates in an order where every gate follows the gates it reads, or a cycle
/// (AIGER-REQ-0012); an explicit stack walks the graph.
fn order(cur: &Cur<'_>, ands: Vec<[u32; 3]>, m: u32, at: u32) -> Result<Vec<[u32; 3]>, Diag> {
    let mut gate_of = vec![u32::MAX; m as usize + 1];
    for (k, g) in ands.iter().enumerate() {
        gate_of[(g[0] >> 1) as usize] = k as u32;
    }
    let mut state = vec![0u8; ands.len()];
    let mut out = Vec::with_capacity(ands.len());
    let mut stack: Vec<(u32, usize)> = Vec::new();
    for start in 0..ands.len() {
        if state[start] != 0 {
            continue;
        }
        state[start] = 1;
        stack.push((start as u32, 1));
        while let Some(top) = stack.last_mut() {
            let (g, c) = *top;
            if c < 3 {
                top.1 += 1;
                let next = gate_of[(ands[g as usize][c] >> 1) as usize];
                if next == u32::MAX {
                    continue;
                }
                match state[next as usize] {
                    0 => {
                        state[next as usize] = 1;
                        stack.push((next, 1));
                    }
                    1 => {
                        return Err(cur.err(
                            at + next,
                            CYCLE,
                            format!(
                                "AND gate {} is part of a combinational cycle",
                                ands[next as usize][0]
                            ),
                        ));
                    }
                    _ => {}
                }
            } else {
                state[g as usize] = 2;
                out.push(ands[g as usize]);
                stack.pop();
            }
        }
    }
    Ok(out)
}

/// The symbol table and the comment section (AIGER-REQ-0023, AIGER-REQ-0024).
fn symbols(cur: &mut Cur<'_>, hd: &Header) -> Result<Symbols, Diag> {
    let mut s = Symbols::default();
    while let Some((l, at)) = cur.next_line() {
        if l == b"c" {
            break;
        }
        let bad = |what: &str| cur.err(at, SYMBOLS, what.to_string());
        let Some((&kind, rest)) = l.split_first() else {
            return Err(bad(
                "an empty line where a symbol or the comment section is expected",
            ));
        };
        let Some(sp) = rest.iter().position(|&b| b == b' ') else {
            return Err(bad(
                "a symbol line is a kind, a position, a space and a name",
            ));
        };
        let Some(pos) = num(&rest[..sp]) else {
            return Err(bad(
                "a symbol line is a kind, a position, a space and a name",
            ));
        };
        let name = &rest[sp + 1..];
        let Ok(name) = std::str::from_utf8(name) else {
            return Err(bad("a symbol name is not UTF-8"));
        };
        if name.is_empty() {
            return Err(bad("a symbol name is empty"));
        }
        let (table, n) = match kind {
            b'i' => (&mut s.inputs, hd.i),
            b'l' => (&mut s.latches, hd.l),
            b'o' => (&mut s.outputs, hd.o),
            b'b' => (&mut s.bad, hd.b),
            b'c' => (&mut s.constraints, hd.c),
            b'j' => (&mut s.justice, hd.j),
            b'f' => (&mut s.fairness, hd.f),
            _ => {
                return Err(bad(
                    "a line after the AND gates is neither a symbol (i, l, o, b, c, j, f) nor 'c'",
                ));
            }
        };
        if pos >= n {
            return Err(bad(&format!(
                "symbol position {pos} is outside its section of {n}"
            )));
        }
        if table.insert(pos, name.to_string()).is_some() {
            return Err(bad(&format!(
                "position {pos} of section '{}' is named twice",
                char::from(kind)
            )));
        }
    }
    Ok(s)
}
