// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Bounded target-library hints for narrow variable-index reads. A four-way
//! mux is compared with four product gates plus their reduction. No names or
//! PDK identities participate. At most 8192 cells/256 six-input truth tables;
//! <1 MiB names/scratch and no graph/path materialization.

use super::{Direction, FxHashMap, Library, glob_match, special_cell_reason};

fn mux4(tt: u64) -> bool {
    // Each of four select cofactors is one (possibly complemented) data pin,
    // hence every real mux is balanced. Reject other functions in one step.
    if tt.count_ones() != 32 {
        return false;
    }
    const VARS: [u16; 4] = [0xaaaa, 0xcccc, 0xf0f0, 0xff00];
    for a in 0..6 {
        for b in a + 1..6 {
            let mut data = [0usize; 4];
            let mut count = 0;
            for i in 0..6 {
                if i != a && i != b {
                    data[count] = i;
                    count += 1;
                }
            }
            for inverted in [false, true] {
                let mut used = 0u8;
                for select in 0..4 {
                    let mut cofactor = 0u16;
                    for values in 0..16 {
                        let mut address = ((select & 1) << a) | (((select >> 1) & 1) << b);
                        for (i, &pin) in data.iter().enumerate() {
                            address |= ((values >> i) & 1) << pin;
                        }
                        cofactor |= (((tt >> address) & 1) as u16) << values;
                    }
                    let Some(pin) = VARS
                        .iter()
                        .position(|v| (if inverted { !*v } else { *v }) == cofactor)
                    else {
                        break;
                    };
                    if used & (1 << pin) != 0 {
                        break;
                    }
                    used |= 1 << pin;
                }
                if used == 15 {
                    return true;
                }
            }
        }
    }
    false
}

pub(crate) fn prefer_narrow_muxes(libs: &[Library], dont_use: &[String]) -> bool {
    if libs
        .iter()
        .fold(0usize, |n, l| n.saturating_add(l.cells.len()))
        > 8192
    {
        return false;
    }
    let mut seen = FxHashMap::default();
    let mut nand = [f64::INFINITY; 2];
    let mut nor = nand;
    let mut and = nand;
    let mut or = nand;
    let mut mux = f64::INFINITY;
    let mut six = 0;
    for cell in libs.iter().flat_map(|l| &l.cells) {
        if seen.insert(cell.name.as_str(), ()).is_some()
            || cell.dont_use
            || !cell.problems.is_empty()
            || special_cell_reason(cell).is_some()
            || !cell.sequentials.is_empty()
            || cell.has_bus
            || cell.has_statetable
            || cell.has_test_cell
            || cell.is_clock_gating
            || !cell.area.is_finite()
            || cell.area < 0.0
            || dont_use.iter().any(|p| glob_match(p, &cell.name))
            || cell.pins.iter().any(|p| p.direction == Direction::Inout)
        {
            continue;
        }
        let inputs: Vec<_> = cell.inputs().map(|p| p.name.as_str()).collect();
        if !matches!(inputs.len(), 2 | 4 | 6) {
            continue;
        }
        let mut outputs = cell.outputs();
        let Some(output) = outputs.next() else {
            continue;
        };
        if outputs.next().is_some() || output.three_state.is_some() {
            continue;
        }
        let Some(tt) = output
            .function
            .as_ref()
            .and_then(|f| f.truth_table(&inputs))
        else {
            continue;
        };
        if inputs.len() == 6 {
            six += 1;
            if six > 256 {
                return false;
            }
            if mux4(tt) {
                mux = mux.min(cell.area);
            }
        } else {
            let slot = usize::from(inputs.len() == 4);
            let all = (1u64 << (1usize << inputs.len())) - 1;
            let high = 1u64 << ((1usize << inputs.len()) - 1);
            if tt == all ^ high {
                nand[slot] = nand[slot].min(cell.area);
            }
            if tt == 1 {
                nor[slot] = nor[slot].min(cell.area);
            }
            if tt == high {
                and[slot] = and[slot].min(cell.area);
            }
            if tt == all ^ 1 {
                or[slot] = or[slot].min(cell.area);
            }
        }
    }
    let products = (4.0 * nand[0] + nand[1])
        .min(4.0 * nor[0] + nor[1])
        .min(4.0 * and[0] + or[1])
        .min(4.0 * or[0] + and[1]);
    mux.is_finite() && products.is_finite() && mux < products - 1e-8
}

#[cfg(test)]
mod tests {
    use super::*;

    // Six inputs, 64 assignments; bounded exhaustive classification checks.
    fn truth(order: [usize; 6], inverted: bool, duplicate: bool) -> u64 {
        (0..64u64).fold(0, |tt, v| {
            let s = ((v >> order[4]) & 1) + 2 * ((v >> order[5]) & 1);
            let d = (if duplicate { s / 2 } else { s }) as usize;
            tt | ((((v >> order[d]) & 1) ^ u64::from(inverted)) << v)
        })
    }

    #[test]
    fn recognizes_pin_permutations_but_not_other_six_input_functions() {
        for order in [[0, 1, 2, 3, 4, 5], [5, 0, 3, 2, 1, 4], [1, 3, 4, 2, 0, 5]] {
            for inverted in [false, true] {
                assert!(mux4(truth(order, inverted, false)));
                assert!(!mux4(truth(order, inverted, true)));
            }
        }
        let parity = (0..64u32).fold(0, |tt, v| tt | (u64::from(v.count_ones() % 2) << v));
        assert!(!mux4(parity));
        assert!(!mux4(0));
        assert!(!mux4(u64::MAX));
    }

    fn library() -> Library {
        rusteda_formats::liberty::read_text(
            r#"
            library(costs) {
              cell(n2) { area: 2; pin(A) { direction: input; } pin(B) { direction: input; }
                pin(Y) { direction: output; function: "!(A & B)"; } }
              cell(n4) { area: 4; pin(A) { direction: input; } pin(B) { direction: input; }
                pin(C) { direction: input; } pin(D) { direction: input; }
                pin(Y) { direction: output; function: "!(A & B & C & D)"; } }
              cell(m4) { area: 10; pin(A) { direction: input; } pin(B) { direction: input; }
                pin(C) { direction: input; } pin(D) { direction: input; }
                pin(S) { direction: input; } pin(T) { direction: input; }
                pin(Y) { direction: output;
                  function: "(!T & !S & A) | (!T & S & B) | (T & !S & C) | (T & S & D)"; } }
            }
        "#,
            "selector-costs",
        )
        .unwrap()
    }

    #[test]
    fn uses_actual_costs_and_honors_cell_exclusions_and_library_priority() {
        let mut lib = library();
        assert!(prefer_narrow_muxes(std::slice::from_ref(&lib), &[]));
        assert!(!prefer_narrow_muxes(
            std::slice::from_ref(&lib),
            &["m*".into()]
        ));
        lib.cells[2].area = 12.0;
        assert!(!prefer_narrow_muxes(std::slice::from_ref(&lib), &[]));
        assert!(!prefer_narrow_muxes(&[lib, library()], &[]));
        let mut lib = library();
        lib.cells[2].dont_use = true;
        assert!(!prefer_narrow_muxes(&[lib], &[]));
    }
}
