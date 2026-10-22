// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Small, bounded arithmetic export for independent external verification.
//! Usage: multiplier_aig WIDTH OUTPUT.aag [signed]
use rustsys::{
    aig::{Aig, Lit},
    bits,
};
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if !(3..=4).contains(&args.len()) {
        return Err("usage: multiplier_aig WIDTH OUTPUT.aag [signed]".into());
    }
    let width: usize = args[1].parse()?;
    if !(1..=32).contains(&width) {
        return Err("width budget: 1..32".into());
    }
    let signed = args.get(3).is_some_and(|a| a == "signed");
    let mut aig = Aig::new();
    let a: Vec<_> = (0..width).map(|_| aig.input()).collect();
    let b: Vec<_> = (0..width).map(|_| aig.input()).collect();
    let a = bits::resize(&a, 2 * width, signed);
    let b = bits::resize(&b, 2 * width, signed);
    let result = if signed {
        bits::mul_signed(&mut aig, &a, &b)
    } else {
        bits::mul(&mut aig, &a, &b)
    };
    let mut file = std::io::BufWriter::new(std::fs::File::create(&args[2])?);
    writeln!(
        file,
        "aag {} {} 0 {} {}",
        aig.num_nodes() - 1,
        aig.num_inputs(),
        result.len(),
        aig.num_ands()
    )?;
    for &input in aig.inputs() {
        writeln!(file, "{}", 2 * input)?;
    }
    for &Lit(output) in &result {
        writeln!(file, "{output}")?;
    }
    for node in 1..aig.num_nodes() as u32 {
        if aig.is_and(node) {
            let (a, b) = aig.fanins(node);
            writeln!(file, "{} {} {}", 2 * node, a.0, b.0)?;
        }
    }
    writeln!(file, "c\nBounded RustSys arithmetic diagnostic")?;
    Ok(())
}
