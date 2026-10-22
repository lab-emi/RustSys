// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Help lines of application and command message codes (CLI-REQ-0006).

/// Code, title and help.
pub const CODES: &[(&str, &str, &str)] = &[
    (
        "A0001",
        "worker threads cannot start",
        "use a smaller --threads value",
    ),
    (
        "A0002",
        "internal failure",
        "this is a defect; please report the command line and the run directory",
    ),
    (
        "A0003",
        "invalid command line",
        "rustsys help lists the commands",
    ),
    (
        "A0004",
        "file cannot be written",
        "check the directory and its permissions",
    ),
    ("A0005", "run or report cannot be read", "check the path"),
    (
        "A0006",
        "unknown or ambiguous run reference",
        "use @, @~N, @<product>, an id prefix of 4 or more characters, or a path",
    ),
    (
        "A0007",
        "run directory unusable",
        "give a new directory, or use --record for a fresh one",
    ),
    (
        "A0008",
        "no runs root",
        "give --runs-root DIR or set RUSTSYS_RUNS_ROOT",
    ),
    (
        "A0009",
        "run of the wrong product or without the needed output",
        "--from-run of check synth takes a synth run; synth takes none",
    ),
    (
        "A0010",
        "view not available for this run",
        "give the run directory; a report file has no run record",
    ),
    (
        "A0020",
        "not a RustSys report or run",
        "give a report JSON file or a run directory",
    ),
    ("A0021", "newer report format", "use a newer rustsys"),
    (
        "A0022",
        "not applicable",
        "the message lists the valid names",
    ),
    (
        "A0023",
        "non-finite number in a report",
        "the value is shown as it is",
    ),
    (
        "A0030",
        "shell not detected",
        "name it, e.g. rustsys completions zsh",
    ),
    ("A0110", "run recorded", ""),
    (
        "A0111",
        "request already committed",
        "the history records a commit of the same command with this --request-id; give a new id to run it again",
    ),
    (
        "A0300",
        "sources of several formats",
        "one run reads SystemVerilog, BLIF (.blif) or one AIGER file (.aag, .aig), chosen by extension",
    ),
    (
        "A0301",
        "option not applicable to the source format",
        "-D, -I, -P, --no-predefined-macros and the case assumptions apply to SystemVerilog; --initial-values to BLIF and AIGER",
    ),
    (
        "A0302",
        "gate-level netlist with other sources",
        "--gate-netlist is the source of the run; give no positional sources with it",
    ),
    (
        "A0303",
        "gate libraries missing or not applicable",
        "--gate-netlist needs --gate-lib; --gate-lib applies to --gate-netlist and to BLIF cells",
    ),
    (
        "C0900",
        "BLIF, AIGER or gate-level source rejected by the checker",
        "the message names the construct; the checker reads the sources that synthesis reads (CHECK-REQ-0100)",
    ),
    (
        "C0901",
        "BLIF construct without effect skipped",
        "timing, clock-constraint and annotation constructs and .exdc do not change the checked function",
    ),
    (
        "C0902",
        "library cell of a BLIF or gate-level source outside the checker's model",
        "the checker evaluates cells with functions, one ff or latch group and one asynchronous control",
    ),
    (
        "C0005",
        "check undecided: a budget ran out",
        "inconclusive, not rejected: the message names the proof or search and its budget; without a difference found the verdict is undecided (exit status 5); --budget and --model-budget raise the conflict budgets",
    ),
    (
        "C0101",
        "high impedance in the netlist",
        "the checker does not model high impedance or drive strengths; check netlists without three-state, open-drain, pull or bus-hold cells, highz drive strengths and switch primitives",
    ),
    (
        "C0102",
        "dual-edge or master-slave cell in the netlist",
        "the checker models flip-flops that sample on a single clock edge and latches with a single enable (no clocked_on_also or enable_also)",
    ),
    (
        "C0103",
        "x or z literal in the netlist",
        "write constants as 1'b0 or 1'b1, or use tie cells",
    ),
    (
        "C0104",
        "cell outside the checker's model",
        "the checker models cells described by function, one ff or latch group, and input and output pins",
    ),
    (
        "C0105",
        "net kind outside the checker's model",
        "the checker models nets with one driver (wire, tri, uwire, variables) and undriven supply0 and supply1 nets",
    ),
    (
        "C0106",
        "RTL statement outside the checker's model",
        "the checker models procedures without wait, disable and event triggers (CHECK-REQ-0014); describe the behavior with clocked processes and state",
    ),
    (
        "S0235",
        "procedural statement not supported for synthesis",
        "wait, disable and event triggers change when statements run (SYNTH-REQ-0133); describe the behavior with clocked processes and state",
    ),
    (
        "S0535",
        "constraint not modelled by the opt-timing estimate",
        "the uniform ideal-clock estimate ignores it; the static timing analysis of the suite applies it",
    ),
    (
        "S0536",
        "timing target not derived from constraints",
        "import SDC constraints with rusteda import --sdc, or give --timing-period and the other target flags",
    ),
    (
        "S0537",
        "targets from the timing table",
        "run the static timing analysis of the suite on the base revision for targets guided by its slack",
    ),
    (
        "S0538",
        "netlist estimate incomplete",
        "the logic depth and delay estimate leave out cells outside the classified library or on combinational cycles",
    ),
    (
        "S0532",
        "cell area target unmet",
        "the produced netlist exceeds the area target; inspect the electrical report",
    ),
    (
        "S0533",
        "estimated timing target unmet",
        "inspect electrical constraints and run timing analysis on the produced netlist",
    ),
    (
        "S0531",
        "invalid electrical constraints",
        "period and area limit must be finite and positive; delays, transitions, load and margin must be finite and nonnegative",
    ),
    (
        "S0913",
        "declared initial values not implemented",
        "--initial-values arbitrary leaves every state uninitialized; the default reset implements them with the generated init port",
    ),
    (
        "S0917",
        "library cell outside the model of gate-level sources",
        "give --gate-lib with the cells; cells need functions, one ff or latch group and connected input pins",
    ),
    (
        "S0703",
        "no output destination",
        "give --out FILE, --run-dir DIR or --record",
    ),
];

/// The help line of a code, when it has one.
pub fn help_for(code: &str) -> Option<&'static str> {
    CODES
        .iter()
        .find(|c| c.0 == code)
        .map(|c| c.2)
        .filter(|h| !h.is_empty())
}

#[cfg(test)]
mod tests {
    // CLI-REQ-0006: one entry per code, well formed.
    #[test]
    fn codes_unique_and_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for (c, t, _) in super::CODES {
            assert!(seen.insert(*c), "duplicate {c}");
            assert!(
                c.len() == 5 && c[1..].bytes().all(|b| b.is_ascii_digit()),
                "{c}"
            );
            assert!(!t.is_empty());
        }
    }

    // CLI-REQ-0004: a message of every code letter (S, C, A, V, B and G) reads back unchanged.
    #[test]
    fn code_letters() {
        // B and G: the BLIF and AIGER readers, whose messages pass through (SYNTH-REQ-0914).
        for c in ['S', 'C', 'A', 'V', 'B', 'G'] {
            let code = format!("{c}0123");
            let d = rusteda_core::job::Diag::warning(code, "x: y").at("f.sv", 3, 1);
            assert_eq!(
                rusteda_core::job::Diag::parse(&d.render_first_line()),
                Some(d)
            );
        }
    }
}
