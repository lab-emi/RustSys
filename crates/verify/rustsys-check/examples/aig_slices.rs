// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Exact bounded output-cone projection of standard combinational binary AIGER.
//! C910: ~29.5M ANDs *8 B +11.9M outputs *4 B =281 MiB. Add <=1M local
//! objects *160 B and bounded stack/IO: <1 GiB planning RSS, one worker under
//! a 4 GiB OS guard. One batch at a time; no paths/all-cones materialization.
//! Every original output belongs to exactly one recorded interval. Both sides
//! share the same compacted input map. Only exact Boolean identities are folded.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
type R<T> = Result<T, Box<dyn std::error::Error>>;

struct Aig {
    inputs: u32,
    ands: Vec<[u32; 2]>,
    outputs: Vec<u32>,
}

fn integer(r: &mut impl Read) -> R<u32> {
    let mut n = 0;
    for shift in (0..35).step_by(7) {
        let mut b = [0];
        r.read_exact(&mut b)?;
        if shift == 28 && b[0] > 15 {
            return Err("AIGER integer overflow".into());
        }
        n |= u32::from(b[0] & 127) << shift;
        if b[0] & 128 == 0 {
            return Ok(n);
        }
    }
    Err("AIGER integer overflow".into())
}
fn line(r: &mut impl BufRead, max: u64) -> R<String> {
    let mut out = String::new();
    (&mut *r).take(max + 1).read_line(&mut out)?;
    if out.len() as u64 > max {
        return Err("AIGER line budget".into());
    }
    Ok(out)
}
fn read(path: &Path) -> R<Aig> {
    if fs::metadata(path)?.len() > 2 * 1024 * 1024 * 1024 {
        return Err("AIGER file byte budget".into());
    }
    let mut r = BufReader::with_capacity(1024 * 1024, File::open(path)?);
    let header = line(&mut r, 256)?;
    let h: Vec<_> = header.split_whitespace().collect();
    if h.len() != 6 || h[0] != "aig" {
        return Err("expected binary combinational AIGER".into());
    }
    let n: Vec<u32> = h[1..].iter().map(|x| x.parse()).collect::<Result<_, _>>()?;
    let (m, i, l, o, a) = (n[0], n[1], n[2], n[3], n[4]);
    if l != 0
        || m != i.checked_add(a).ok_or("AIGER count overflow")?
        || i > 2_000_000
        || a > 64_000_000
        || o == 0
        || o > 8_000_000
        || m >= u32::MAX / 2
    {
        return Err("AIGER graph/interface budget".into());
    }
    let mut outputs = Vec::new();
    outputs.try_reserve_exact(o as usize)?;
    for _ in 0..o {
        let x: u32 = line(&mut r, 32)?.trim().parse()?;
        if x / 2 > m {
            return Err("bad output literal".into());
        }
        outputs.push(x);
    }
    let mut ands = Vec::new();
    ands.try_reserve_exact(a as usize)?;
    for k in 0..a {
        let lhs = 2 * (i + k + 1);
        let x = lhs.checked_sub(integer(&mut r)?).ok_or("bad AIGER delta")?;
        let y = x.checked_sub(integer(&mut r)?).ok_or("bad AIGER delta")?;
        if x / 2 >= lhs / 2 || y > x {
            return Err("AIGER is not strictly topological".into());
        }
        ands.push([x, y]);
    }
    Ok(Aig {
        inputs: i,
        ands,
        outputs,
    })
}

#[derive(Clone, Copy)]
enum Node {
    Input { original: u32, index: u32 },
    And(u32, u32),
}
struct Project {
    images: [HashMap<u32, u32>; 2],
    inputs: HashMap<u32, u32>,
    ands: HashMap<(u32, u32), u32>,
    nodes: Vec<Node>,
    stack: Vec<(u32, bool)>,
    work: u64,
    max_work: u64,
    max_nodes: usize,
}
impl Project {
    fn clear(&mut self) {
        for map in &mut self.images {
            map.clear();
        }
        self.inputs.clear();
        self.ands.clear();
        self.nodes.clear();
        self.stack.clear();
    }
    fn add(&mut self, node: Node) -> R<u32> {
        if self.nodes.len() >= self.max_nodes {
            return Err("SLICE_NODE_BUDGET".into());
        }
        self.nodes.push(node);
        Ok(2 * self.nodes.len() as u32)
    }
    fn and(&mut self, a: u32, b: u32) -> R<u32> {
        if a == 0 || b == 0 || a == (b ^ 1) {
            return Ok(0);
        }
        if a == 1 || a == b {
            return Ok(b);
        }
        if b == 1 {
            return Ok(a);
        }
        let key = (a.min(b), a.max(b));
        if let Some(&lit) = self.ands.get(&key) {
            return Ok(lit);
        }
        let lit = self.add(Node::And(key.0, key.1))?;
        self.ands.insert(key, lit);
        Ok(lit)
    }
    fn known(&self, side: usize, lit: u32) -> u32 {
        if lit < 2 {
            lit
        } else {
            self.images[side][&(lit / 2)] ^ (lit & 1)
        }
    }
    fn resolve(&mut self, a: &Aig, side: usize, root: u32) -> R<u32> {
        self.stack.push((root, false));
        while let Some((lit, post)) = self.stack.pop() {
            self.work += 1;
            if self.work > self.max_work {
                return Err("SLICE_WORK_BUDGET".into());
            }
            if lit < 2 {
                continue;
            }
            let v = lit / 2;
            if self.images[side].contains_key(&v) {
                continue;
            }
            if v <= a.inputs {
                let local = if let Some(&x) = self.inputs.get(&v) {
                    x
                } else {
                    let index = self.inputs.len() as u32;
                    let x = self.add(Node::Input { original: v, index })?;
                    self.inputs.insert(v, x);
                    x
                };
                self.images[side].insert(v, local);
                continue;
            }
            let [x, y] = a.ands[(v - a.inputs - 1) as usize];
            if post {
                let result = self.and(self.known(side, x), self.known(side, y))?;
                self.images[side].insert(v, result);
            } else {
                if self.stack.len() > 3 * self.max_nodes {
                    return Err("SLICE_NODE_BUDGET".into());
                }
                self.stack.extend([(lit, true), (x, false), (y, false)]);
            }
        }
        Ok(self.known(side, root))
    }
}
fn put(w: &mut impl Write, mut n: u32) -> R<()> {
    loop {
        let b = (n & 127) as u8;
        n >>= 7;
        w.write_all(&[if n == 0 { b } else { b | 128 }])?;
        if n == 0 {
            break;
        }
    }
    Ok(())
}
fn write(folder: &Path, p: &Project, outputs: &[(u32, [u32; 2])]) -> R<(u64, usize)> {
    fs::create_dir(folder)?;
    let inputs = p.inputs.len() as u32;
    let mut image = vec![0u32; p.nodes.len() + 1];
    let mut gates = Vec::new();
    let mut input_ids = vec![0u32; inputs as usize];
    for (k, node) in p.nodes.iter().enumerate() {
        let lit = match node {
            Node::Input { original, index } => {
                input_ids[*index as usize] = *original;
                2 * (index + 1)
            }
            Node::And(a, b) => {
                let get = |x: u32| {
                    if x < 2 {
                        x
                    } else {
                        image[(x / 2) as usize] ^ (x & 1)
                    }
                };
                let (a, b) = (get(*a), get(*b));
                gates.push([a.max(b), a.min(b)]);
                2 * (inputs + gates.len() as u32)
            }
        };
        image[k + 1] = lit;
    }
    let mut map = BufWriter::new(File::create_new(folder.join("input_ids.u32le"))?);
    for v in input_ids {
        map.write_all(&v.to_le_bytes())?;
    }
    map.flush()?;
    let mut ids = BufWriter::new(File::create_new(folder.join("output_ids.u32le"))?);
    for (i, _) in outputs {
        ids.write_all(&i.to_le_bytes())?;
    }
    ids.flush()?;
    for side in 0..2 {
        let mut w = BufWriter::with_capacity(
            1024 * 1024,
            File::create_new(folder.join(if side == 0 { "gold.aig" } else { "gate.aig" }))?,
        );
        writeln!(
            w,
            "aig {} {inputs} 0 {} {}",
            inputs + gates.len() as u32,
            outputs.len(),
            gates.len()
        )?;
        for (_, roots) in outputs {
            let r = roots[side];
            let value = if r < 2 {
                r
            } else {
                image[(r / 2) as usize] ^ (r & 1)
            };
            writeln!(w, "{value}")?;
        }
        for (k, [a, b]) in gates.iter().enumerate() {
            let lhs = 2 * (inputs + k as u32 + 1);
            if *a >= lhs || b > a {
                return Err("unordered slice projection".into());
            }
            put(&mut w, lhs - a)?;
            put(&mut w, a - b)?;
        }
        w.flush()?;
    }
    let bytes = [
        "gold.aig",
        "gate.aig",
        "input_ids.u32le",
        "output_ids.u32le",
    ]
    .iter()
    .map(|n| fs::metadata(folder.join(n)).map(|x| x.len()))
    .collect::<Result<Vec<_>, _>>()?
    .into_iter()
    .sum();
    Ok((bytes, gates.len()))
}

fn main() -> R<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(3..=7).contains(&args.len()) {
        return Err("usage: aig_slices GOLD GATE OUTPUT_DIR [BATCH_OUTPUTS [MAX_NODES [MAX_WORK [MAX_DISK_BYTES]]]]".into());
    }
    let number = |i: usize, default: u64| -> R<u64> {
        args.get(i).map_or(Ok(default), |v| {
            Ok(v.to_str().ok_or("invalid budget")?.parse()?)
        })
    };
    let batch = number(3, 4096)? as usize;
    let max_nodes = number(4, 1_000_000)? as usize;
    let max_work = number(5, 4_000_000_000)?;
    let max_disk = number(6, 8 * 1024 * 1024 * 1024)?;
    if batch == 0
        || batch > 16384
        || max_nodes == 0
        || max_nodes > 2_000_000
        || max_work == 0
        || max_work > 20_000_000_000
        || max_disk > 32 * 1024 * 1024 * 1024
    {
        return Err("invalid slice budgets".into());
    }
    let output = PathBuf::from(&args[2]);
    fs::create_dir(&output)?;
    let result = (|| -> R<serde_json::Value> {
        let a = read(Path::new(&args[0]))?;
        let b = read(Path::new(&args[1]))?;
        if a.inputs != b.inputs || a.outputs.len() != b.outputs.len() {
            return Err("original AIGER interfaces differ".into());
        }
        let total = a.outputs.len();
        let mut pending: Vec<_> = (0..total)
            .step_by(batch)
            .map(|start| (start, (start + batch).min(total)))
            .collect();
        pending.reverse();
        let mut p = Project {
            images: [HashMap::new(), HashMap::new()],
            inputs: HashMap::new(),
            ands: HashMap::new(),
            nodes: Vec::new(),
            stack: Vec::new(),
            work: 0,
            max_work,
            max_nodes,
        };
        let mut slices = Vec::new();
        let mut disk = 0u64;
        let mut direct_total = 0;
        let mut identity_total = 0;
        let mut projected_total = 0;
        while let Some((start, end)) = pending.pop() {
            if slices.len() + pending.len() >= 65536 {
                return Err("slice count budget".into());
            }
            p.clear();
            let mut selected = Vec::new();
            let mut direct = 0;
            let mut identities = 0;
            let trial = (|| -> R<()> {
                for i in start..end {
                    let (x, y) = (a.outputs[i], b.outputs[i]);
                    if x == y && x / 2 <= a.inputs {
                        direct += 1;
                        continue;
                    }
                    let roots = [p.resolve(&a, 0, x)?, p.resolve(&b, 1, y)?];
                    if roots[0] == roots[1] {
                        identities += 1;
                    } else {
                        selected.push((i as u32, roots));
                    }
                }
                Ok(())
            })();
            if let Err(error) = trial {
                if error.to_string() == "SLICE_NODE_BUDGET" && end - start > 1 {
                    let mid = start + (end - start) / 2;
                    pending.push((mid, end));
                    pending.push((start, mid));
                    continue;
                }
                return Err(error);
            }
            let id = slices.len();
            let folder = output.join(format!("slice{id:05}"));
            let mut ands = 0;
            if !selected.is_empty() {
                let (bytes, n) = write(&folder, &p, &selected)?;
                ands = n;
                disk += bytes;
                if disk > max_disk {
                    return Err("slice workspace byte budget".into());
                }
            }
            direct_total += direct;
            identity_total += identities;
            projected_total += selected.len();
            slices.push(serde_json::json!({"id":id,"start":start,"end":end,"direct":direct,"structural":identities,"cec_outputs":selected.len(),"inputs":p.inputs.len(),"ands":ands}));
            if id % 64 == 0 {
                eprintln!(
                    "slices {}, outputs {end}/{total}, projected {projected_total}, identities {}, expansions {}",
                    slices.len(),
                    direct_total + identity_total,
                    p.work
                );
            }
        }
        Ok(
            serde_json::json!({"status":"prepared","equivalence":"not_run","original_inputs":a.inputs,"original_outputs":total,
            "direct_identities":direct_total,"structural_identities":identity_total,"projected_outputs":projected_total,"slices":slices,
            "source_graphs_unchanged":true,"input_map":"same original input ids on both sides; unused inputs have no fanin path to the selected outputs",
            "projection":"exact AND/NOT DAG; constant, idempotence, complement and commutativity identities only",
            "expansions":p.work,"max_expansions":max_work,"max_nodes_per_slice":max_nodes,"max_slices":65536,"max_disk_bytes":max_disk,"disk_bytes":disk}),
        )
    })();
    let report = match result {
        Ok(x) => x,
        Err(e) => {
            serde_json::json!({"status":"unverified","equivalence":"unverified","diagnostic":e.to_string()})
        }
    };
    let mut w = BufWriter::new(File::create_new(output.join("manifest.json"))?);
    serde_json::to_writer_pretty(&mut w, &report)?;
    w.flush()?;
    eprintln!("{}", report["status"]);
    Ok(())
}
