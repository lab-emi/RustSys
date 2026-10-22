// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Binary heap of unassigned variables ordered by activity (VSIDS order) for the solver.

use super::Solver;

impl Solver {
    /// Higher activity first; ties go to the lower index, so that decisions limited to a prefix
    /// of the variables rarely meet later ones.
    pub(super) fn heap_less(&self, a: u32, b: u32) -> bool {
        let (x, y) = (self.activity[a as usize], self.activity[b as usize]);
        x > y || (x == y && a < b)
    }

    pub(super) fn heap_insert(&mut self, v: u32) {
        self.heap_pos[v as usize] = self.heap.len() as i64;
        self.heap.push(v);
        let i = self.heap.len() - 1;
        self.heap_up(i);
    }

    pub(super) fn heap_up(&mut self, mut i: usize) {
        let v = self.heap[i];
        while i > 0 {
            let parent = (i - 1) / 2;
            if !self.heap_less(v, self.heap[parent]) {
                break;
            }
            self.heap[i] = self.heap[parent];
            self.heap_pos[self.heap[i] as usize] = i as i64;
            i = parent;
        }
        self.heap[i] = v;
        self.heap_pos[v as usize] = i as i64;
    }

    pub(super) fn heap_down(&mut self, mut i: usize) {
        let v = self.heap[i];
        let n = self.heap.len();
        loop {
            let l = 2 * i + 1;
            if l >= n {
                break;
            }
            let r = l + 1;
            let c = if r < n && self.heap_less(self.heap[r], self.heap[l]) {
                r
            } else {
                l
            };
            if !self.heap_less(self.heap[c], v) {
                break;
            }
            self.heap[i] = self.heap[c];
            self.heap_pos[self.heap[i] as usize] = i as i64;
            i = c;
        }
        self.heap[i] = v;
        self.heap_pos[v as usize] = i as i64;
    }

    pub(super) fn heap_pop(&mut self) -> Option<u32> {
        if self.heap.is_empty() {
            return None;
        }
        let top = self.heap[0];
        self.heap_pos[top as usize] = -1;
        let last = self.heap.pop().expect("non-empty");
        if !self.heap.is_empty() {
            self.heap[0] = last;
            self.heap_pos[last as usize] = 0;
            self.heap_down(0);
        }
        Some(top)
    }
}
