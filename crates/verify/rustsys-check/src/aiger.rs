// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The checker's own AIGER reader (CHECK-REQ-0102, CHECK-REQ-0107): the ASCII and the binary
//! form with the version 1.9 header, latch reset values, symbols and comments
//! (AIGER-REQ-0010 to AIGER-REQ-0024), read from the AIGER specification and built straight into
//! circuit variables with the ports and names of SYNTH-REQ-0903 to SYNTH-REQ-0907.

use std::collections::HashMap;
use std::path::Path;

use crate::Input;
use crate::blif::assemble;
use crate::circ::{B, Circuit};
use crate::rtl::RtlState;
use crate::sources::{InitialValues, SOURCE, SourceModel, bytes};

/// The largest maximum variable index the checker reads (CHECK-REQ-0107).
const MAX_VARS: u64 = 1 << 24;

/// The bytes of a file with a position and the current line.
struct Rd<'b> {
    b: &'b [u8],
    i: usize,
    line: u32,
    name: &'b str,
}

impl<'b> Rd<'b> {
    fn err(&self, msg: impl std::fmt::Display) -> String {
        format!(
            "error[{SOURCE}]: {}:{}:1: {msg}",
            self.name,
            self.line.max(1)
        )
    }

    /// The next line without its line feed (and a carriage return before it).
    fn text_line(&mut self) -> Option<&'b [u8]> {
        if self.i >= self.b.len() {
            return None;
        }
        let rest = &self.b[self.i..];
        let end = rest.iter().position(|&x| x == b'\n').unwrap_or(rest.len());
        self.i += (end + 1).min(rest.len());
        self.line += 1;
        let l = &rest[..end];
        Some(l.strip_suffix(b"\r").unwrap_or(l))
    }

    /// The numbers of the next line; `n` lists how many it may hold.
    fn nums(&mut self, n: &[usize]) -> Result<Vec<u64>, String> {
        let l = self
            .text_line()
            .ok_or_else(|| self.err("the file ends early"))?;
        let mut out = Vec::new();
        for w in l.split(|&x| x == b' ') {
            if w.is_empty() || w.len() > 10 || !w.iter().all(u8::is_ascii_digit) {
                return Err(self.err("a line of decimal numbers separated by single spaces"));
            }
            out.push(w.iter().fold(0u64, |a, &d| a * 10 + u64::from(d - b'0')));
        }
        if !n.contains(&out.len()) {
            return Err(self.err(format!("{} numbers on a line that takes {n:?}", out.len())));
        }
        Ok(out)
    }

    /// One number of the binary AND gates: 7 bits per byte, the least significant first.
    fn delta(&mut self) -> Result<u64, String> {
        let mut x = 0u64;
        for k in 0..5 {
            let Some(&byte) = self.b.get(self.i) else {
                return Err(self.err("the binary AND gates end early"));
            };
            self.i += 1;
            x |= u64::from(byte & 0x7f) << (7 * k);
            if byte & 0x80 == 0 {
                return if x > u64::from(u32::MAX) {
                    Err(self.err("a binary difference above 32 bits"))
                } else {
                    Ok(x)
                };
            }
        }
        Err(self.err("a binary difference of more than five bytes"))
    }
}

/// Variables of the circuit for AIGER variables, made when first named.
struct Vars {
    of: Vec<u32>,
    defined: Vec<bool>,
}

impl Vars {
    fn var(&mut self, c: &mut Circuit, v: usize) -> u32 {
        if self.of[v] == u32::MAX {
            self.of[v] = c.free();
        }
        self.of[v]
    }

    fn lit(&mut self, c: &mut Circuit, l: u64) -> B {
        let v = (l >> 1) as usize;
        let b = if v == 0 { B::F } else { B::V(self.var(c, v)) };
        if l & 1 == 1 { c.not(b) } else { b }
    }

    /// Marks the variable of even literal `l` defined.
    fn define(&mut self, rd: &Rd<'_>, l: u64, m: u64) -> Result<usize, String> {
        if l < 2 || l & 1 == 1 || l > 2 * m {
            return Err(rd.err(format!("{l} is not an even literal of a variable")));
        }
        let v = (l >> 1) as usize;
        if self.defined[v] {
            return Err(rd.err(format!("variable {v} is defined twice")));
        }
        self.defined[v] = true;
        Ok(v)
    }
}

/// Builds the model of an AIGER file into `c` (CHECK-REQ-0102).
pub(crate) fn build(
    c: &mut Circuit,
    input: &Input,
    top: Option<&str>,
    mode: InitialValues,
) -> Result<SourceModel, String> {
    let data = bytes(input)?;
    let (name, b) = data
        .first()
        .ok_or_else(|| format!("error[{SOURCE}]: no AIGER file"))?;
    let mut rd = Rd {
        b,
        i: 0,
        line: 0,
        name,
    };
    let head = rd.text_line().unwrap_or_default();
    let words: Vec<&[u8]> = head.split(|&x| x == b' ').collect();
    let binary = match words.first() {
        Some(&b"aig") => true,
        Some(&b"aag") => false,
        _ => return Err(rd.err("not an AIGER header")),
    };
    let mut n = [0u64; 9];
    if !(6..=10).contains(&words.len()) {
        return Err(rd.err("a header of 5 to 9 numbers"));
    }
    for (k, w) in words[1..].iter().enumerate() {
        if w.is_empty() || w.len() > 10 || !w.iter().all(u8::is_ascii_digit) {
            return Err(rd.err("a header number"));
        }
        n[k] = w.iter().fold(0u64, |a, &d| a * 10 + u64::from(d - b'0'));
    }
    let [m, ni, nl, no, na, nb, nc, nj, nf] = n;
    if nb + nc + nj + nf > 0 {
        return Err(
            rd.err("bad-state, constraint, justice and fairness properties are not synthesizable")
        );
    }
    if m > MAX_VARS || m < ni + nl + na || (binary && m != ni + nl + na) {
        return Err(rd.err(format!("M = {m} for I + L + A = {}", ni + nl + na)));
    }
    let lines = nl + no + if binary { 0 } else { ni + na };
    let rest = (b.len() - rd.i) as u64;
    if 2 * lines + if binary { 2 * na } else { 0 } > rest + 1 {
        return Err(rd.err("the header announces more than the file holds"));
    }
    let mut vars = Vars {
        of: vec![u32::MAX; m as usize + 1],
        defined: vec![false; m as usize + 1],
    };
    let in_range = |rd: &Rd<'_>, l: u64| {
        if l > 2 * m + 1 {
            Err(rd.err(format!("literal {l} above 2M + 1")))
        } else {
            Ok(l)
        }
    };
    let mut inputs = Vec::new();
    for k in 0..ni {
        let l = if binary {
            2 * (k + 1)
        } else {
            rd.nums(&[1])?[0]
        };
        let v = vars.define(&rd, l, m)?;
        inputs.push(vars.var(c, v));
    }
    let mut latches = Vec::new();
    for k in 0..nl {
        let w = if binary {
            let mut w = vec![2 * (ni + k + 1)];
            w.extend(rd.nums(&[1, 2])?);
            w
        } else {
            rd.nums(&[2, 3])?
        };
        let v = vars.define(&rd, w[0], m)?;
        let next = in_range(&rd, w[1])?;
        let init = match w.get(2) {
            None | Some(0) => Some(false),
            Some(1) => Some(true),
            Some(r) if *r == w[0] => None,
            Some(r) => return Err(rd.err(format!("latch reset {r}"))),
        };
        latches.push((vars.var(c, v), next, init));
    }
    let mut outputs = Vec::new();
    for _ in 0..no {
        let l = rd.nums(&[1])?[0];
        let l = in_range(&rd, l)?;
        outputs.push(vars.lit(c, l));
    }
    let mut ands = Vec::new();
    for k in 0..na {
        let (lhs, a, bb) = if binary {
            let lhs = 2 * (ni + nl + k + 1);
            let d0 = rd.delta()?;
            let d1 = rd.delta()?;
            if d0 == 0 || d0 > lhs || d1 > lhs - d0 {
                return Err(rd.err(format!("binary AND gate {k} reads at or above itself")));
            }
            (lhs, lhs - d0, lhs - d0 - d1)
        } else {
            let w = rd.nums(&[3])?;
            (w[0], in_range(&rd, w[1])?, in_range(&rd, w[2])?)
        };
        let v = vars.define(&rd, lhs, m)?;
        ands.push((v, a, bb));
    }
    for (v, a, bb) in ands {
        let x = vars.lit(c, a);
        let y = vars.lit(c, bb);
        let g = c.and(x, y);
        let var = vars.var(c, v);
        c.define(var, g);
    }
    let latches: Vec<(u32, B, Option<bool>)> = latches
        .into_iter()
        .map(|(q, next, init)| (q, vars.lit(c, next), init))
        .collect();
    if let Some(v) = (1..vars.of.len()).find(|&v| vars.of[v] != u32::MAX && !vars.defined[v]) {
        return Err(rd.err(format!("variable {v} is read but nothing defines it")));
    }
    // Symbols and comments (AIGER-REQ-0023, AIGER-REQ-0024).
    let mut sym: HashMap<(u8, u64), String> = HashMap::new();
    while let Some(l) = rd.text_line() {
        if l == b"c" {
            break;
        }
        let ok = l.len() > 2 && matches!(l[0], b'i' | b'l' | b'o');
        let sp = l.iter().position(|&x| x == b' ');
        let (Some(sp), true) = (sp, ok) else {
            return Err(rd.err("a symbol line i<k>, l<k> or o<k> and a name, or 'c'"));
        };
        let pos = std::str::from_utf8(&l[1..sp])
            .ok()
            .filter(|t| !t.is_empty() && t.bytes().all(|x| x.is_ascii_digit()))
            .and_then(|t| t.parse::<u64>().ok());
        let limit = match l[0] {
            b'i' => ni,
            b'l' => nl,
            _ => no,
        };
        let name = String::from_utf8_lossy(&l[sp + 1..]).into_owned();
        match pos {
            Some(p) if p < limit && !name.is_empty() => {
                if sym.insert((l[0], p), name).is_some() {
                    return Err(rd.err("a position named twice"));
                }
            }
            _ => return Err(rd.err("a symbol outside its section")),
        }
    }
    let named = |k: u8, p: usize| {
        sym.get(&(k, p as u64))
            .cloned()
            .unwrap_or_else(|| format!("{}{p}", char::from(k)))
    };
    let clock = (nl > 0).then(|| c.free());
    let mut init = None;
    let mut declared = Vec::new();
    let mut states = Vec::new();
    for (k, (q, next, value)) in latches.into_iter().enumerate() {
        let n = named(b'l', k);
        let arst = match value {
            Some(v) => {
                declared.push((n.clone(), v));
                match mode {
                    InitialValues::Reset => Some((B::V(*init.get_or_insert_with(|| c.free())), v)),
                    InitialValues::Arbitrary => None,
                }
            }
            None => None,
        };
        states.push(RtlState {
            name: n,
            latch: false,
            q,
            next,
            clk: B::V(clock.unwrap_or_default()),
            arst,
        });
    }
    let ins = inputs
        .into_iter()
        .enumerate()
        .map(|(k, v)| (named(b'i', k), v))
        .collect();
    let outs = outputs
        .into_iter()
        .enumerate()
        .map(|(k, b)| (named(b'o', k), b))
        .collect();
    let module = top.map_or_else(
        || {
            Path::new(name)
                .file_stem()
                .map_or_else(|| name.clone(), |s| s.to_string_lossy().into_owned())
        },
        str::to_string,
    );
    let model = assemble(c, module, ins, outs, states, clock, init, &|_| None)?;
    Ok(SourceModel {
        model,
        initial_values: declared,
        warnings: Vec::new(),
    })
}
