// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The one table of option help texts (PROTO-REQ-0010), used by the command line's help.

/// An option: flag and help.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionDef {
    pub flag: &'static str,
    pub help: &'static str,
}

macro_rules! option_defs {
    ($($id:ident = ($flag:expr, $help:expr);)*) => {
        $(pub const $id: OptionDef = OptionDef { flag: $flag, help: $help };)*
        /// Every option, in table order.
        pub const ALL: &[OptionDef] = &[$($id),*];
    };
}

option_defs! {
    TOP = ("--top", "Top module (default: the one module no other module instantiates)");
    DEFINE = ("-D", "Define a macro: NAME or NAME=VALUE (repeatable)");
    INCLUDE = ("-I", "Include directory (repeatable)");
    PARAM = ("-P", "Override a top-level parameter: NAME=EXPR (repeatable)");
    AFTER_RESET = ("--after-reset", "Assume scalar inputs are asserted together before observation: NAME=0|1 (repeatable); enable proved constant-state removal");
    UNINITIALIZED_DONT_CARE = ("--uninitialized-dont-care", "Allow proved constant choices for unreset flop initial states; reports initial-state refinement, not arbitrary-initial-state equivalence");
    ASSUME_FULL_CASE = ("--assume-full-case", "Treat case statements marked complete (full_case, unique, priority) as complete: without a default item, the last item acts as the default");
    ASSUME_PARALLEL_CASE = ("--assume-parallel-case", "Treat case statements marked parallel (parallel_case, unique, unique0) as parallel: items match without priority and overlapping items OR their results");
    NO_PREDEFINED = ("--no-predefined-macros", "Do not predefine the SYNTHESIS macro");
    LIB = ("--lib", "Liberty library (repeatable)");
    SOURCES = ("SOURCE", "Source files: SystemVerilog, BLIF (.blif) or AIGER (.aag, .aig), chosen by extension");
    GATE_NETLIST = ("--gate-netlist", "Structural netlist of --gate-lib cells as the source (synth) or the reference (check synth)");
    GATE_LIB = ("--gate-lib", "Liberty library of the cells of --gate-netlist and of BLIF .gate and .mlatch (repeatable)");
    INITIAL_VALUES = ("--initial-values", "What declared initial values of BLIF and AIGER state become: reset (an asynchronous control from a generated init port, default) or arbitrary (not implemented)");
    DONT_USE = ("--dont-use", "Exclude cells by name, a glob with * and ? (repeatable)");
    EFFORT = ("--effort", "Optimization and mapping effort: low, medium or high");
    GOAL = ("--goal", "What mapping minimizes first: delay or area");
    DELAY_RELAX = ("--delay-relax", "Allowed relative delay increase during area recovery, finite and not negative");
    ELECTRICAL_CORNER = ("--electrical-corner", "Additional same-cell Liberty corner for capacitance and transition repair (repeatable; requires timing period)");
    TIMING_PERIOD = ("--timing-period", "Enable load-aware refinement for this positive ideal-clock period in ns");
    AREA_LIMIT = ("--area-limit", "Target maximum total cell area in library units; feasibility is reported");
    INPUT_DELAY = ("--input-delay", "Uniform input arrival delay in ns");
    OUTPUT_DELAY = ("--output-delay", "Uniform output delay budget in ns");
    INPUT_SLEW = ("--input-slew", "Uniform input transition time in ns");
    CLOCK_SLEW = ("--clock-slew", "Ideal-clock transition time in ns");
    OUTPUT_LOAD = ("--output-load", "Capacitance added to each output port bit in pF");
    TIMING_MARGIN = ("--timing-margin", "Additional setup margin deducted from the target period, in ns");
    BUFFER_FANOUT = ("--buffer-fanout", "Target sinks per buffer branch, from 2 to 128; area and timing budgets may limit insertion");
    BACKEND = ("--backend", "Where the parallel phases run: cpu, gpu (the first usable GPU runtime) or cuda");
    OUT = ("--out", "Output netlist (default with a run directory: netlist.v in it)");
    REPORT = ("--report", "JSON report (default with a run directory: report.json in it)");
    NETLIST = ("--netlist", "Structural Verilog netlist (repeatable)");
    FROM_RUN = ("--from-run", "Take inputs from a finished run: a run directory or a run reference such as @synth");
    SEED = ("--seed", "Seed of the random simulation");
    CYCLES = ("--cycles", "Clock cycles of sequential simulation of both designs (0: off)");
    BUDGET = ("--budget", "SAT conflict budget of each proof obligation of the check; a proof that reaches it leaves the check undecided");
    MODEL_BUDGET = ("--model-budget", "SAT conflict budget of each proof that the checker's RTL model needs; one that reaches it leaves the check undecided");
    SWEEP_MAX_CANDIDATES = ("--sweep-max-candidates", "Maximum optional sweeping candidates (0: structural hashing only); all final proof obligations still run");
    SUPPLY_PORT = ("--supply-port", "Netlist port carrying power or ground, left out of the comparison (repeatable)");
    RUN_DIR = ("--run-dir", "Record the run in this directory (it must not hold a run yet)");
    RECORD = ("--record", "Record the run in a new directory under the runs root");
    LABEL = ("--label", "Free text stored with the run");
    PRINT = ("--print", "What standard output carries: text, json or none");
}

/// Looks an option up by flag.
pub fn by_flag(flag: &str) -> Option<&'static OptionDef> {
    ALL.iter().find(|o| o.flag == flag)
}
