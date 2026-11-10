// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Extraction of the gates of a cover.

use super::*;

impl<'a> Mapper<'a> {
    /// The gates of the current cover in (node, phase) order, with the logic depth.
    /// Intermediate-only view: counts contain the same pins/roots as `extract`, without gate
    /// strings, input Vec allocations or depth work. The placeholder never leaves mapping.
    pub(in crate::map) fn extract_current(&mut self) -> Mapped {
        if !self.compact_cover {
            return self.extract();
        }
        let (_, used) = self.cover_sweep(None);
        let mut refs = vec![[0u32; 2]; self.aig.num_nodes()];
        for (id, used) in used.into_iter().enumerate() {
            for (p, used) in used.into_iter().enumerate() {
                if !used {
                    continue;
                }
                match self.best[id][p].imp {
                    Impl::Inv => refs[id][1 - p] += 1,
                    Impl::Cell { cut, m } => {
                        let c = &self.cuts[id][cut as usize];
                        let cell = &self.lib.comb[m.cell as usize];
                        for i in 0..cell.n_inputs as usize {
                            let leaf = c.leaves[m.perm[i] as usize] as usize;
                            refs[leaf][usize::from((m.phase >> i) & 1 == 1)] += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
        for r in self.roots {
            if !r.is_const() {
                refs[r.node() as usize][usize::from(r.is_compl())] += 1;
            }
        }
        self.current_refs = Some(refs);
        Mapped {
            gates: Vec::new(),
            free_neg_used: Default::default(),
            max_arrival: 0.0,
            depth: 0,
            backend: "cpu",
            phase_s: Vec::new(),
        }
    }

    /// The final gates of the cover in (node, phase) order, including exact logic depth.
    pub(in crate::map) fn extract(&self) -> Mapped {
        let (_, used) = self.cover_sweep(None);
        let n = self.aig.num_nodes();
        let gates: Vec<MappedGate> = (0..n as u32)
            .into_par_iter()
            .flat_map_iter(|id| {
                let this = &*self;
                let used = used[id as usize];
                [false, true].into_iter().filter_map(move |ph| {
                    if !used[usize::from(ph)] {
                        return None;
                    }
                    match this.best[id as usize][usize::from(ph)].imp {
                        Impl::Input | Impl::FreeNeg => None,
                        Impl::Inv => Some(MappedGate {
                            cell: this.lib.inverter,
                            out: (id, ph),
                            ins: vec![(id, !ph)],
                        }),
                        Impl::Cell { cut, m } => {
                            let c = &this.cuts[id as usize][cut as usize];
                            let cell = &this.lib.comb[m.cell as usize];
                            Some(MappedGate {
                                cell: m.cell as usize,
                                out: (id, ph),
                                ins: (0..cell.inputs.len())
                                    .map(|i| {
                                        (c.leaves[m.perm[i] as usize], (m.phase >> i) & 1 == 1)
                                    })
                                    .collect(),
                            })
                        }
                        Impl::None => {
                            // Every AND node has a two-input decomposition through the inverter
                            // and a NAND/AND/NOR-like cell (checked when the library is
                            // classified).
                            unreachable!("node {id} phase {ph} has no implementation")
                        }
                    }
                })
            })
            .collect();
        let free_neg_used = self
            .aig
            .inputs()
            .iter()
            .copied()
            .filter(|&id| {
                (0..2)
                    .any(|p| used[id as usize][p] && self.best[id as usize][p].imp == Impl::FreeNeg)
            })
            .collect();
        // Logic depth: nodes are topologically numbered; within a node the inverter of the other
        // phase comes after the phase it reads.
        let mut level = vec![[0u32; 2]; n];
        let mut depth = 0u32;
        let mut i = 0;
        while i < gates.len() {
            let id = gates[i].out.0;
            let mut j = i;
            while j < gates.len() && gates[j].out.0 == id {
                j += 1;
            }
            for inv in [false, true] {
                for g in &gates[i..j] {
                    if g.ins.first().is_some_and(|x| x.0 == id) != inv {
                        continue;
                    }
                    let d = 1 + g
                        .ins
                        .iter()
                        .map(|x| level[x.0 as usize][usize::from(x.1)])
                        .max()
                        .unwrap_or(0);
                    level[id as usize][usize::from(g.out.1)] = d;
                    depth = depth.max(d);
                }
            }
            i = j;
        }
        Mapped {
            gates,
            free_neg_used,
            max_arrival: 0.0,
            depth,
            backend: "cpu",
            phase_s: Vec::new(),
        }
    }
}
