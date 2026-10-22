// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The cut-pass task (COMPUTE-REQ-0101 to COMPUTE-REQ-0106): one priority-cut pass of a
//! dual-phase standard-cell mapper over an and-inverter graph, level by level. For every node of a
//! level, the feasible merges of its fanins' cut pairs (plus, in passes with required times, the
//! cuts chosen by the previous pass) are deduplicated, ranked by the pass cost, the best
//! `max_cuts` are kept, and the best library match of both phases is chosen, with an inverter
//! from the other phase as an alternative. All costs are integers.
//!
//! A [`CutPassBackend`] opens a [`CutPassSession`] on a [`CutPassProblem`]; the session keeps the
//! cut table and the choices resident between passes, so an engine runs its delay pass and its
//! area-flow passes on one session and reads back only what it needs after each pass. Every
//! backend returns word-identical results for the same problem and pass sequence
//! (COMPUTE-REQ-0104).

mod cpu;
#[cfg(feature = "cuda")]
mod cuda;
mod pack;
#[cfg(test)]
mod tests;

use crate::{BackendKind, ComputeError, GpuRuntime};

/// Largest number of priority cuts per node (the cut slot of an [`Imp`] has 7 bits and the
/// node's trivial cut takes one slot after the kept cuts).
pub const MAX_CUTS: u32 = 126;
/// Largest number of library matches (the match index of an [`Imp`] has 22 bits).
pub const MAX_MATCHES: usize = 1 << 22;
/// Largest number of cut leaves.
pub const MAX_LEAVES: u32 = 6;
/// Arrival of an unmapped (node, phase); finite arrivals stay below it.
pub const ARR_INF: i32 = i32::MAX;
/// Saturation value of area flows, and the area flow of an unmapped (node, phase).
pub const AF_MAX: u32 = 1 << 30;

/// Implementation code of a (node, phase): the kind in bits 0-2; for a cell, the slot of its cut
/// in bits 3-9 and the index of its match in bits 10-31.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Imp(pub u32);

impl Imp {
    /// Not computed.
    pub const NONE: Imp = Imp(0);
    /// Combinational input, positive phase.
    pub const INPUT: Imp = Imp(1);
    /// Free complemented output of an input's driver.
    pub const FREE_NEG: Imp = Imp(2);
    /// Inverter from the other phase.
    pub const INV: Imp = Imp(3);
    /// Kind of a cell implementation.
    pub const KIND_CELL: u32 = 4;

    /// A cell over cut slot `cut` with match `m`.
    pub fn cell(cut: u32, m: u32) -> Imp {
        Imp(Imp::KIND_CELL | (cut << 3) | (m << 10))
    }
    pub fn kind(self) -> u32 {
        self.0 & 7
    }
    pub fn cut(self) -> u32 {
        (self.0 >> 3) & 0x7F
    }
    pub fn match_index(self) -> u32 {
        self.0 >> 10
    }
}

/// Implementation, arrival and area flow of one (node, phase).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub imp: Imp,
    /// Arrival in fixed-point delay units; [`ARR_INF`] when unmapped.
    pub arr: i32,
    /// Area flow in fixed-point area units, at most [`AF_MAX`].
    pub af: u32,
}

impl Choice {
    pub const NONE: Choice = Choice {
        imp: Imp::NONE,
        arr: ARR_INF,
        af: AF_MAX,
    };
}

/// A cut: `n` sorted leaf nodes (unused leaves 0), the root function as a six-variable truth
/// table where variable i is leaf i, and the leaf signature (bit `leaf % 64` of every leaf).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CutRow {
    pub leaves: [u32; 6],
    pub n: u32,
    pub tt: u64,
    pub sign: u64,
}

/// The library side of the problem: match keys `(vars, tt)` in ascending order without repeats,
/// each with its matches in preference order (CSR), and the cells the matches refer to.
#[derive(Debug, Clone, Copy)]
pub struct MatchTable<'a> {
    /// Number of variables of each key, 1 to 6.
    pub key_vars: &'a [u8],
    /// Truth table of each key over its variables (bits above `2^vars` clear).
    pub key_tt: &'a [u64],
    /// Matches of key i: `key_first[i]..key_first[i + 1]` (length: keys + 1).
    pub key_first: &'a [u32],
    /// Cell of each match.
    pub m_cell: &'a [u32],
    /// Cell input i of a match connects to leaf `perm[i]`.
    pub m_perm: &'a [[u8; 6]],
    /// Cell input i of a match is inverted when bit i is set.
    pub m_phase: &'a [u8],
    /// Input count of each cell (equal to the variables of the keys of its matches).
    pub c_inputs: &'a [u8],
    /// Area of each cell in fixed-point units, at most [`AF_MAX`].
    pub c_area: &'a [u32],
    /// Delay from each input of each cell in fixed-point units, not negative.
    pub c_delay: &'a [[i32; 6]],
}

/// A mapping problem: the graph, the initial choices and the library.
#[derive(Debug, Clone, Copy)]
pub struct CutPassProblem<'a> {
    /// The fanin literals (`2 * node + complemented`) of every AND node; `[0, 0]` for other nodes.
    /// Node 0 is the constant.
    pub fanins: &'a [[u32; 2]],
    /// The nodes that start with their trivial cut (combinational inputs), ascending.
    pub inputs: &'a [u32],
    /// The AND nodes to compute, grouped by level: level i is
    /// `level_nodes[level_ptr[i]..level_ptr[i + 1]]`; every fanin of a node is an input or a node
    /// of an earlier level.
    pub level_ptr: &'a [u32],
    pub level_nodes: &'a [u32],
    /// The choices before the first pass, per node and phase (no cell implementations).
    pub init: &'a [[Choice; 2]],
    /// Largest cut size `k`, 1 to 6.
    pub max_leaves: u32,
    /// Priority cuts kept per node, 1 to [`MAX_CUTS`].
    pub max_cuts: u32,
    /// Delay and area of the inverter of the inverter alternative.
    pub inv_delay: i32,
    pub inv_area: u32,
    pub lib: MatchTable<'a>,
}

/// The cost that ranks the candidate cuts of a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rank {
    /// Arrival, then leaf count, then area flow.
    Delay,
    /// Area flow, then leaf count, then arrival.
    AreaFlow,
}

/// The inputs of one pass.
#[derive(Debug, Clone, Copy)]
pub struct PassCosts<'a> {
    pub rank: Rank,
    /// Half fanouts per node (the divisor of area-flow shares; values below 2 count as 2).
    pub fo2: &'a [u32],
    /// Required time per node and phase; [`ARR_INF`] marks a phase outside the cover. With
    /// required times, the cuts that the previous pass chose for the used phases stay candidates.
    pub required: Option<&'a [[i32; 2]]>,
}

/// What a pass reads back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readback {
    /// The choices and the chosen cut of every (node, phase).
    Chosen,
    /// The choices and the whole cut table.
    All,
}

/// The result of a pass.
#[derive(Debug, Clone, Default)]
pub struct PassOutput {
    /// Per node: the choices of both phases.
    pub choices: Vec<[Choice; 2]>,
    /// [`Readback::Chosen`]: `rows[2 * node + phase]` is the cut of that (node, phase) when a cell
    /// implements it, else all zero. [`Readback::All`]: `slots` rows per node, of which the first
    /// `cut_count[node]` are the node's cuts (the kept cuts in rank order, then the trivial cut).
    pub rows: Vec<CutRow>,
    /// [`Readback::All`] only: cuts per node.
    pub cut_count: Vec<u32>,
    /// Rows per node in the cut table: `max_cuts + 1`.
    pub slots: u32,
}

/// What a pass cost (wall clock).
#[derive(Debug, Clone, Copy, Default)]
pub struct PassStats {
    /// Kernel launches (GPU) or levels (CPU).
    pub launches: u32,
    /// Until every node of the pass is computed and every error is collected.
    pub kernel_s: f64,
    /// Copying the results into the [`PassOutput`].
    pub readback_s: f64,
}

/// A backend of the cut-pass task.
pub trait CutPassBackend: Send + Sync {
    fn name(&self) -> &'static str;
    /// Validates the problem (COMPUTE-REQ-0101) and makes it resident.
    fn open<'s>(
        &'s self,
        p: &CutPassProblem<'_>,
    ) -> Result<Box<dyn CutPassSession + 's>, ComputeError>;
}

/// A problem resident on a backend; passes run in call order.
pub trait CutPassSession {
    /// Runs one pass over every level and reads back `rb` into `out`.
    fn run(
        &mut self,
        c: &PassCosts<'_>,
        rb: Readback,
        out: &mut PassOutput,
    ) -> Result<PassStats, ComputeError>;
}

/// The backend for `kind` (COMPUTE-REQ-0004): `Cpu` always; `Cuda` when compiled and its device
/// passes the self-test; `Gpu` the first compiled GPU backend whose device passes it. A backend
/// that is missing or fails is an error, never a fallback to another backend.
pub fn backend(kind: BackendKind) -> Result<&'static dyn CutPassBackend, ComputeError> {
    match kind {
        BackendKind::Cpu => Ok(&cpu::CPU),
        BackendKind::Cuda => {
            if !GpuRuntime::Cuda.compiled() {
                return Err(ComputeError::NotCompiled("cuda"));
            }
            crate::prepare(GpuRuntime::Cuda)
                .map_err(|e| ComputeError::Unavailable(format!("cuda: {}", e.0)))?;
            gpu_backend(GpuRuntime::Cuda)
        }
        BackendKind::Gpu => {
            let compiled: Vec<GpuRuntime> = GpuRuntime::ALL
                .into_iter()
                .filter(|r| r.compiled())
                .collect();
            if compiled.is_empty() {
                return Err(ComputeError::Unavailable(
                    "this build has no GPU backend; enable the `cuda` feature".into(),
                ));
            }
            let mut failures = Vec::new();
            for rt in compiled {
                match crate::prepare(rt) {
                    Ok(()) => return gpu_backend(rt),
                    Err(e) => failures.push(format!("{}: {}", rt.name(), e.0)),
                }
            }
            Err(ComputeError::Unavailable(failures.join("; ")))
        }
    }
}

fn gpu_backend(rt: GpuRuntime) -> Result<&'static dyn CutPassBackend, ComputeError> {
    match rt {
        #[cfg(feature = "cuda")]
        GpuRuntime::Cuda => Ok(&cuda::CUDA),
        #[allow(unreachable_patterns)]
        _ => Err(ComputeError::NotCompiled(rt.name())),
    }
}

/// Truth-table mask of a function of `vars` variables.
fn tt_mask(vars: u32) -> u64 {
    if vars >= 6 {
        u64::MAX
    } else {
        (1u64 << (1u32 << vars)) - 1
    }
}

/// Checks the input contract of COMPUTE-REQ-0101. Kernels index device arrays with the values
/// checked here, so a GPU backend never launches on a problem that fails it.
pub fn validate(p: &CutPassProblem<'_>) -> Result<(), ComputeError> {
    let bad = |m: String| Err(ComputeError::Invalid(m));
    let n = p.fanins.len();
    if n == 0 || p.init.len() != n {
        return bad(format!("{n} nodes with {} initial choices", p.init.len()));
    }
    if n > (u32::MAX >> 2) as usize {
        return Err(ComputeError::TooLarge(format!("{n} nodes")));
    }
    if !(1..=MAX_LEAVES).contains(&p.max_leaves) {
        return bad(format!("cut size {}", p.max_leaves));
    }
    if p.max_cuts == 0 || p.max_cuts > MAX_CUTS {
        return Err(ComputeError::TooLarge(format!(
            "{} priority cuts per node: the cut-pass task supports 1 to {MAX_CUTS}",
            p.max_cuts
        )));
    }
    if p.inv_delay < 0 || p.inv_area > AF_MAX {
        return bad("inverter delay or area out of range".into());
    }
    for c in p.init {
        for x in c {
            if x.imp.kind() >= Imp::KIND_CELL || x.af > AF_MAX {
                return bad("an initial choice is a cell or has an area flow above AF_MAX".into());
            }
        }
    }
    // Topological levels: inputs are ready; a level's nodes read only ready nodes.
    let mut ready = vec![false; n];
    for &i in p.inputs {
        if i as usize >= n || i == 0 || ready[i as usize] {
            return bad(format!("input {i}"));
        }
        ready[i as usize] = true;
    }
    if p.level_ptr.first() != Some(&0)
        || p.level_ptr.last().map(|x| *x as usize) != Some(p.level_nodes.len())
        || p.level_ptr.windows(2).any(|w| w[0] > w[1])
    {
        return bad("level offsets".into());
    }
    for w in p.level_ptr.windows(2) {
        let level = &p.level_nodes[w[0] as usize..w[1] as usize];
        for &id in level {
            if id as usize >= n || id == 0 || ready[id as usize] {
                return bad(format!("level node {id}"));
            }
            for l in p.fanins[id as usize] {
                let f = (l >> 1) as usize;
                if f == 0 || f >= n || !ready[f] {
                    return bad(format!("fanin {f} of node {id} is not computed before it"));
                }
            }
        }
        for &id in level {
            ready[id as usize] = true;
        }
    }
    // Library.
    let t = &p.lib;
    let keys = t.key_vars.len();
    let matches = t.m_cell.len();
    let cells = t.c_inputs.len();
    if t.key_tt.len() != keys
        || t.key_first.len() != keys + 1
        || t.key_first.first() != Some(&0)
        || t.key_first.last().map(|x| *x as usize) != Some(matches)
        || t.key_first.windows(2).any(|w| w[0] > w[1])
        || t.m_perm.len() != matches
        || t.m_phase.len() != matches
        || t.c_area.len() != cells
        || t.c_delay.len() != cells
    {
        return bad("match table shape".into());
    }
    if matches > MAX_MATCHES {
        return Err(ComputeError::TooLarge(format!(
            "{matches} library matches: the cut-pass task supports at most {MAX_MATCHES}"
        )));
    }
    for c in 0..cells {
        let k = t.c_inputs[c] as usize;
        if !(1..=6).contains(&k) || t.c_area[c] > AF_MAX || t.c_delay[c][..k].iter().any(|d| *d < 0)
        {
            return bad(format!("cell {c}"));
        }
    }
    for i in 0..keys {
        let vars = u32::from(t.key_vars[i]);
        if !(1..=6).contains(&vars) || t.key_tt[i] & !tt_mask(vars) != 0 {
            return bad(format!("key {i}"));
        }
        if i > 0 && (t.key_vars[i - 1], t.key_tt[i - 1]) >= (t.key_vars[i], t.key_tt[i]) {
            return bad("keys are not strictly ascending".into());
        }
        for m in t.key_first[i] as usize..t.key_first[i + 1] as usize {
            let cell = t.m_cell[m] as usize;
            if cell >= cells
                || u32::from(t.c_inputs[cell]) != vars
                || t.m_perm[m][..vars as usize]
                    .iter()
                    .any(|x| u32::from(*x) >= vars)
                || u32::from(t.m_phase[m]) >> vars != 0
            {
                return bad(format!("match {m}"));
            }
        }
    }
    Ok(())
}

/// Checks the inputs of one pass against the problem size.
fn check_costs(n: usize, c: &PassCosts<'_>) -> Result<(), ComputeError> {
    if c.fo2.len() != n || c.required.is_some_and(|r| r.len() != n) {
        return Err(ComputeError::Invalid(format!(
            "pass costs for {} nodes, problem has {n}",
            c.fo2.len()
        )));
    }
    Ok(())
}
