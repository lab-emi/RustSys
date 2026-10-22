// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Integration tests for the bounded deterministic Tcl subset of `rusteda_formats::sdc`
//! (SDC-REQ-0001, SDC-REQ-0002, SDC-REQ-0007 to 0015, SDC-REQ-0040).

use std::path::PathBuf;

use rusteda_core::diag::Severity;
use rusteda_formats::sdc::{self, CmdKind, Env, Limits, MinMaxScope, Obj, Source};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("verification/fixtures/synthetic/sdc")
}

// SDC-REQ-0001, SDC-REQ-0002, SDC-REQ-0007, SDC-REQ-0008, SDC-REQ-0009: comments, line
// continuations, semicolon separators, variable reads/writes, unset, global `::` qualifier, `{*}`
// expansion, and `$::env(VAR)` / `$env(VAR)` reads and writes without consulting `std::env::var`.
#[test]
fn lexical_variables_and_env_dictionary() {
    let env = Env::from_pairs([("SEED_PERIOD", "4.0")]);
    let limits = Limits::default();
    let script = r#"
# Leading comment
set ::clk_name "core_clk" ; # semicolon comment
set p \
    $::env(SEED_PERIOD)
set ::env(DERIVED_CAP) 0.05
set cap $env(DERIVED_CAP)
create_clock -name ${::clk_name} -period $p [get_ports clk_i]
set load_opts [list -pin_load -max $cap]
set_load {*}$load_opts [get_ports dout]
unset p
if {![info exists p]} {
    set_logic_zero [get_ports tie_lo]
}
"#;
    let sources = [Source::new("test.sdc", script)];
    let (sdc, diags) = sdc::read(&sources, &env, &limits);
    assert!(diags.is_empty(), "unexpected diags: {diags:?}");
    assert_eq!(sdc.cmds.len(), 3);
    assert!(matches!(
        &sdc.cmds[0].kind,
        CmdKind::CreateClock { name: Some(n), period, .. }
            if n == "core_clk" && (*period - 4.0).abs() < 1e-12
    ));
    assert!(matches!(
        &sdc.cmds[1].kind,
        CmdKind::SetLoad { value, pin_load: true, min_max: MinMaxScope::Max, .. }
            if (*value - 0.05).abs() < 1e-12
    ));
    assert!(matches!(&sdc.cmds[2].kind, CmdKind::SetLogicZero { .. }));
    assert!(
        sdc.env
            .iter()
            .any(|e| e.name == "DERIVED_CAP" && e.value == "0.05" && e.written)
    );
}

// SDC-REQ-0007, SDC-REQ-0008, SDC-REQ-0009: unterminated delimiters emit `D0001`, and reading an
// unset variable or unset `$::env(MISSING)` emits `D0007`.
#[test]
fn syntax_d0001_and_unset_variable_d0007() {
    let (_, d_syntax) = sdc::read_str("set x \"unterminated\n");
    assert!(d_syntax.iter().any(|d| d.code == "D0001"), "{d_syntax:?}");

    let (_, diags) = sdc::read_str("create_clock -name clk -period $missing_var [get_ports clk]\n");
    assert!(diags.iter().any(|d| d.code == "D0007"), "{diags:?}");

    let (_, diags_env) =
        sdc::read_str("create_clock -name clk -period $::env(NO_SUCH_ENV_KEY) [get_ports clk]\n");
    assert!(diags_env.iter().any(|d| d.code == "D0007"), "{diags_env:?}");
}

// SDC-REQ-0010: `[expr {...}]` arithmetic, comparisons, ternary, string equality, and math
// functions (`abs`, `min`, `max`, `int`, `double`, `round`), plus division/modulo by zero `D0005`.
#[test]
fn expr_evaluator_and_math_functions() {
    let script = r#"
set p [expr {1.0 + 2 * 1.5}]
set half [expr {$p / 2.0}]
set clamped [expr {max(0.5, min(2.0, abs(-1.25)))}]
set rounded [expr {round(3.6) + int(2.9) + double(1)}]
set ratio [expr {5 / 100}]
set cond_val [expr {("fast" eq "fast") && (5 > 3) ? $clamped : 9.0}]
create_clock -name clk -period $p -waveform [list 0.0 $half] [get_ports clk]
set_input_delay -clock [get_clocks clk] -max $cond_val [get_ports din]
set_output_delay -clock [get_clocks clk] -max [expr {$rounded * 0.1 + $ratio}] [get_ports dout]
"#;
    let (sdc, diags) = sdc::read_str(script);
    assert!(diags.is_empty(), "unexpected diags: {diags:?}");
    assert_eq!(sdc.cmds.len(), 3);
    assert!(matches!(
        &sdc.cmds[0].kind,
        CmdKind::CreateClock { period, waveform: Some([r, f]), .. }
            if (*period - 4.0).abs() < 1e-12
                && r.abs() < 1e-12
                && (*f - 2.0).abs() < 1e-12
    ));
    assert!(matches!(
        &sdc.cmds[1].kind,
        CmdKind::SetInputDelay { delay, .. } if (*delay - 1.25).abs() < 1e-12
    ));
    assert!(matches!(
        &sdc.cmds[2].kind,
        CmdKind::SetOutputDelay { delay, .. } if (*delay - 0.75).abs() < 1e-12
    ));

    let (_, div_zero) = sdc::read_str("set x [expr {10 / 0}]\n");
    assert!(div_zero.iter().any(|d| d.code == "D0005"), "{div_zero:?}");
}

// SDC-REQ-0011: `if` / `elseif` / `else` with short-circuiting `[info exists ::env(X)]` so
// guarded `$::env(X)` references on untaken branches never fault.
#[test]
fn conditional_short_circuit_with_info_exists() {
    let script = r#"
if {[info exists ::env(OPT_PERIOD)] && $::env(OPT_PERIOD) > 0} {
    set period $::env(OPT_PERIOD)
} elseif {[info exists ::env(FALLBACK_PERIOD)]} {
    set period $::env(FALLBACK_PERIOD)
} else {
    set period 5.0
}
create_clock -name clk -period $period [get_ports clk]
"#;
    let (sdc_default, diags_default) = sdc::read_str(script);
    assert!(diags_default.is_empty(), "{diags_default:?}");
    assert!(matches!(
        &sdc_default.cmds[0].kind,
        CmdKind::CreateClock { period, .. } if (*period - 5.0).abs() < 1e-12
    ));

    let env = Env::from_pairs([("OPT_PERIOD", "2.0")]);
    let sources = [Source::new("cond.sdc", script)];
    let (sdc_opt, diags_opt) = sdc::read(&sources, &env, &Limits::default());
    assert!(diags_opt.is_empty(), "{diags_opt:?}");
    assert!(matches!(
        &sdc_opt.cmds[0].kind,
        CmdKind::CreateClock { period, .. } if (*period - 2.0).abs() < 1e-12
    ));
}

// SDC-REQ-0011: `foreach` over a plain Tcl list works; `foreach` over an object query (`[all_inputs]`,
// `[get_ports *]`) emits `D0006`.
#[test]
fn foreach_plain_list_and_d0006_on_object_collection() {
    let ok_script = r#"
create_clock -name clk -period 2.0 [get_ports clk]
foreach p {din0 din1 din2} {
    set_input_delay -clock [get_clocks clk] 0.4 [get_ports $p]
}
"#;
    let (sdc, diags) = sdc::read_str(ok_script);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(sdc.cmds.len(), 4);

    let bad_script = r#"
create_clock -name clk -period 2.0 [get_ports clk]
foreach input [all_inputs] {
    set_input_delay -clock [get_clocks clk] 0.4 $input
}
"#;
    let (_, bad_diags) = sdc::read_str(bad_script);
    assert!(
        bad_diags.iter().any(|d| {
            d.code == "D0006"
                && d.severity == Severity::Error
                && d.location.as_ref().map(|l| l.line) == Some(3)
        }),
        "{bad_diags:?}"
    );
}

// SDC-REQ-0014: `puts` and `puts stderr`/`stdout` emit `D0040` (`Severity::Note`).
#[test]
fn puts_emits_d0040_note() {
    let script = r#"
puts "loading timing constraints"
puts -nonewline stderr "warning note"
create_clock -name clk -period 2.0 [get_ports clk]
"#;
    let (sdc, diags) = sdc::read_str(script);
    assert_eq!(sdc.cmds.len(), 1);
    let info_diags: Vec<_> = diags.iter().filter(|d| d.code == "D0040").collect();
    assert_eq!(info_diags.len(), 2, "{diags:?}");
    assert!(info_diags.iter().all(|d| d.severity == Severity::Note));
    assert!(info_diags[0].message.contains("loading timing constraints"));
    assert!(info_diags[1].message.contains("warning note"));
}

// SDC-REQ-0012, SDC-REQ-0013: list, string, and collection manipulation (`list`, `concat`,
// `llength`, `lindex`, `lappend`, `split`, `join`, `string`, `add_to_collection`,
// `remove_from_collection`, and `lsearch` + `lreplace` port-exclusion idiom).
#[test]
fn list_string_and_collection_commands() {
    let script = r#"
set base [list a b]
lappend base c
set extra [concat $base [list clk]]
set idx [lsearch $extra clk]
set filtered [lreplace $extra $idx $idx]
set count [llength $filtered]
set first_port [lindex $filtered 0]
set joined [join $filtered "_"]
set parts [split "clk:rst" ":"]
if {[string match -nocase "CLK*" [lindex $parts 0]] && [string equal $first_port "a"] && [string length $joined] == 5} {
    create_clock -name $joined -period [expr {$count * 1.0}] [get_ports clk_i]
}
set col [add_to_collection [get_ports $first_port] [get_ports [lindex $parts 1]]]
set col2 [remove_from_collection $col [get_ports rst]]
set_input_delay -clock [get_clocks a_b_c] 0.5 $col2
"#;
    let (sdc, diags) = sdc::read_str(script);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(sdc.cmds.len(), 2);
    assert!(matches!(
        &sdc.cmds[0].kind,
        CmdKind::CreateClock { name: Some(n), period, sources: Some(src), .. }
            if n == "a_b_c"
                && (*period - 3.0).abs() < 1e-12
                && src == &Obj::ports(["clk_i"])
    ));
    assert!(matches!(
        &sdc.cmds[1].kind,
        CmdKind::SetInputDelay { ports_or_pins, .. }
            if ports_or_pins
                == &Obj::diff(
                    Obj::union(vec![Obj::ports(["a"]), Obj::ports(["rst"])]),
                    Obj::ports(["rst"]),
                )
    ));
}

// SDC-REQ-0014: relative `source` within the parent directory works; `..` escape, absolute paths,
// and cyclic includes emit `D0006`.
#[test]
fn relative_source_and_d0006_guards() {
    let dir = fixture_dir();
    let path = dir.join("tcl_subset.sdc");
    let text = std::fs::read_to_string(&path).expect("read tcl_subset.sdc");
    let sources = [Source::new(path.to_string_lossy().into_owned(), text)];
    let (sdc, diags) = sdc::read(&sources, &Env::default(), &Limits::default());
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(sdc.cmds.len(), 5);
    assert_eq!(sdc.sources.len(), 2);

    // `..` traversal and absolute paths are rejected with `D0006`.
    let (_, d_parent) = sdc::read_str("source ../sdc/sourced_clocks.sdc\n");
    assert!(d_parent.iter().any(|d| d.code == "D0006"), "{d_parent:?}");

    let (_, d_abs) = sdc::read_str("source /etc/hosts\n");
    assert!(d_abs.iter().any(|d| d.code == "D0006"), "{d_abs:?}");

    // Cyclic virtual source emits `D0008` (recursion depth limit) or `D0006`.
    let mut limits = Limits {
        max_depth: 8,
        ..Limits::default()
    };
    limits
        .virtual_sources
        .insert("cycle.sdc".to_string(), "source cycle.sdc\n".to_string());
    let (_, d_cycle) = sdc::read(
        &[Source::new("top.sdc", "source cycle.sdc\n")],
        &Env::default(),
        &limits,
    );
    assert!(
        d_cycle
            .iter()
            .any(|d| d.code == "D0008" || d.code == "D0006"),
        "{d_cycle:?}"
    );
}

// SDC-REQ-0015: step budget (`max_steps`) and depth budget (`max_depth`) emit `D0008`, and
// unsupported Tcl constructs emit `D0006`.
#[test]
fn resource_budgets_d0008_and_unsupported_tcl_d0006() {
    let limits_steps = Limits {
        max_steps: 5,
        ..Limits::default()
    };
    let many = "set a 1\nset b 2\nset c 3\nset d 4\nset e 5\nset f 6\nset g 7\n";
    let (_, d_steps) = sdc::read(
        &[Source::new("steps.sdc", many)],
        &Env::default(),
        &limits_steps,
    );
    assert!(d_steps.iter().any(|d| d.code == "D0008"), "{d_steps:?}");

    let limits_depth = Limits {
        max_depth: 3,
        ..Limits::default()
    };
    let deep = "set x [list [list [list [list [list a]]]]]\n";
    let (_, d_depth) = sdc::read(
        &[Source::new("depth.sdc", deep)],
        &Env::default(),
        &limits_depth,
    );
    assert!(d_depth.iter().any(|d| d.code == "D0008"), "{d_depth:?}");

    for kw in [
        "proc foo {} {}",
        "while {0} {}",
        "for {set i 0} {$i < 1} {incr i} {}",
        "switch a { a {} }",
        "eval {set x 1}",
        "namespace eval ns {}",
        "regexp a b",
        "open foo r",
        "exec ls",
        "catch {set x 1}",
    ] {
        let (_, diags) = sdc::read_str(kw);
        assert!(
            diags.iter().any(|d| d.code == "D0006"),
            "expected D0006 for `{kw}`, got {diags:?}"
        );
    }
}
