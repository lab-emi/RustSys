// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Canonical, variable-free SDC writer (`write(&Sdc) -> String`; SDC-REQ-0003).

use super::Sdc;
use super::cmd::{
    ClockGroupsKind, CmdKind, EarlyLateScope, EdgeScope, ExceptionEnd, MinMaxScope, SetupHoldScope,
    StartEndScope,
};
use super::obj::{Obj, ObjQuery};

/// Writes `sdc` in canonical, variable-free SDC syntax such that `read(write(s)) == s`
/// (SDC-REQ-0003).
pub fn write_sdc(sdc: &Sdc) -> String {
    let mut out = String::new();
    for cmd in &sdc.cmds {
        write_cmd_kind(&mut out, &cmd.kind);
        out.push('\n');
    }
    out
}

fn write_word(out: &mut String, s: &str) {
    if is_safe_bare(s) {
        out.push_str(s);
    } else if is_safe_braced(s) {
        out.push('{');
        out.push_str(s);
        out.push('}');
    } else {
        out.push('"');
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '$' => out.push_str("\\$"),
                '[' => out.push_str("\\["),
                ']' => out.push_str("\\]"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                other => out.push(other),
            }
        }
        out.push('"');
    }
}

fn is_safe_bare(s: &str) -> bool {
    if s.is_empty() || s.starts_with('-') {
        return false;
    }
    s.bytes().all(|b| {
        b.is_ascii_alphanumeric()
            || matches!(b, b'_' | b'.' | b'/' | b'*' | b'?' | b':' | b'+' | b'-')
    })
}

fn is_safe_braced(s: &str) -> bool {
    !s.is_empty()
        && !s
            .bytes()
            .any(|b| matches!(b, b'\\' | b'{' | b'}' | b'\n' | b'\r'))
}

fn write_f64(out: &mut String, v: f64) {
    let v = if v == 0.0 { 0.0 } else { v };
    out.push_str(&format!("{v}"));
}

fn write_edge_flags(out: &mut String, edge: EdgeScope) {
    match edge {
        EdgeScope::Both => {}
        EdgeScope::Rise => out.push_str(" -rise"),
        EdgeScope::Fall => out.push_str(" -fall"),
    }
}

fn write_min_max_flags(out: &mut String, mm: MinMaxScope) {
    match mm {
        MinMaxScope::Both => {}
        MinMaxScope::Min => out.push_str(" -min"),
        MinMaxScope::Max => out.push_str(" -max"),
    }
}

fn write_setup_hold_flags(out: &mut String, sh: SetupHoldScope) {
    match sh {
        SetupHoldScope::Both => {}
        SetupHoldScope::Setup => out.push_str(" -setup"),
        SetupHoldScope::Hold => out.push_str(" -hold"),
    }
}

fn write_early_late_flags(out: &mut String, el: EarlyLateScope) {
    match el {
        EarlyLateScope::Both => {}
        EarlyLateScope::Early => out.push_str(" -early"),
        EarlyLateScope::Late => out.push_str(" -late"),
    }
}

fn write_start_end_flags(out: &mut String, se: StartEndScope) {
    match se {
        StartEndScope::Default => {}
        StartEndScope::Start => out.push_str(" -start"),
        StartEndScope::End => out.push_str(" -end"),
    }
}

fn write_from_end(out: &mut String, end: &Option<ExceptionEnd>) {
    if let Some(e) = end {
        match e.edge {
            EdgeScope::Both => out.push_str(" -from "),
            EdgeScope::Rise => out.push_str(" -rise_from "),
            EdgeScope::Fall => out.push_str(" -fall_from "),
        }
        write_obj(out, &e.obj);
    }
}

fn write_through_ends(out: &mut String, ends: &[ExceptionEnd]) {
    for e in ends {
        match e.edge {
            EdgeScope::Both => out.push_str(" -through "),
            EdgeScope::Rise => out.push_str(" -rise_through "),
            EdgeScope::Fall => out.push_str(" -fall_through "),
        }
        write_obj(out, &e.obj);
    }
}

fn write_to_end(out: &mut String, end: &Option<ExceptionEnd>) {
    if let Some(e) = end {
        match e.edge {
            EdgeScope::Both => out.push_str(" -to "),
            EdgeScope::Rise => out.push_str(" -rise_to "),
            EdgeScope::Fall => out.push_str(" -fall_to "),
        }
        write_obj(out, &e.obj);
    }
}

/// Formats an [`Obj`] query in canonical SDC syntax.
pub fn write_obj(out: &mut String, obj: &Obj) {
    match obj {
        Obj::Ports(q) => write_obj_query(out, "get_ports", q),
        Obj::Pins(q) => write_obj_query(out, "get_pins", q),
        Obj::Cells(q) => write_obj_query(out, "get_cells", q),
        Obj::Nets(q) => write_obj_query(out, "get_nets", q),
        Obj::Clocks(q) => write_obj_query(out, "get_clocks", q),
        Obj::LibCells(q) => write_obj_query(out, "get_lib_cells", q),
        Obj::LibPins(q) => write_obj_query(out, "get_lib_pins", q),
        Obj::AllInputs { no_clocks } => {
            if *no_clocks {
                out.push_str("[all_inputs -no_clocks]");
            } else {
                out.push_str("[all_inputs]");
            }
        }
        Obj::AllOutputs => out.push_str("[all_outputs]"),
        Obj::AllClocks => out.push_str("[all_clocks]"),
        Obj::AllRegisters => out.push_str("[all_registers]"),
        Obj::CurrentDesign => out.push_str("[current_design]"),
        Obj::Names(names) => {
            if names.len() == 1 {
                write_word(out, &names[0]);
            } else if names
                .iter()
                .all(|n| !n.is_empty() && !n.chars().any(char::is_whitespace) && is_safe_braced(n))
            {
                out.push('{');
                for (i, n) in names.iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    out.push_str(n);
                }
                out.push('}');
            } else {
                out.push_str("[list");
                for n in names {
                    out.push(' ');
                    write_word(out, n);
                }
                out.push(']');
            }
        }
        Obj::Union(items) => {
            out.push_str("[list");
            for item in items {
                out.push(' ');
                write_obj(out, item);
            }
            out.push(']');
        }
        Obj::Diff(lhs, rhs) => {
            out.push_str("[remove_from_collection ");
            write_obj(out, lhs);
            out.push(' ');
            write_obj(out, rhs);
            out.push(']');
        }
    }
}

fn write_obj_query(out: &mut String, cmd: &str, q: &ObjQuery) {
    out.push('[');
    out.push_str(cmd);
    if q.hierarchical {
        out.push_str(" -hierarchical");
    }
    if q.regexp {
        out.push_str(" -regexp");
    }
    if q.nocase {
        out.push_str(" -nocase");
    }
    if q.quiet {
        out.push_str(" -quiet");
    }
    if let Some(of) = &q.of_objects {
        out.push_str(" -of_objects ");
        write_obj(out, of);
    }
    if let Some(flt) = &q.filter {
        out.push_str(" -filter ");
        write_word(out, &flt.to_canonical());
    }
    if q.patterns.len() == 1 {
        out.push(' ');
        write_word(out, &q.patterns[0]);
    } else if q.patterns.len() > 1 {
        out.push_str(" [list");
        for p in &q.patterns {
            out.push(' ');
            write_word(out, p);
        }
        out.push(']');
    }
    out.push(']');
}

fn write_cmd_kind(out: &mut String, kind: &CmdKind) {
    match kind {
        CmdKind::SetUnits {
            time,
            capacitance,
            resistance,
            voltage,
            current,
            power,
        } => {
            out.push_str("set_units");
            if let Some(t) = time {
                out.push_str(" -time ");
                write_word(out, t);
            }
            if let Some(c) = capacitance {
                out.push_str(" -capacitance ");
                write_word(out, c);
            }
            if let Some(r) = resistance {
                out.push_str(" -resistance ");
                write_word(out, r);
            }
            if let Some(v) = voltage {
                out.push_str(" -voltage ");
                write_word(out, v);
            }
            if let Some(i) = current {
                out.push_str(" -current ");
                write_word(out, i);
            }
            if let Some(p) = power {
                out.push_str(" -power ");
                write_word(out, p);
            }
        }
        CmdKind::CurrentDesign { name } => {
            out.push_str("current_design ");
            write_word(out, name);
        }
        CmdKind::SetOperatingConditions {
            condition,
            min,
            max,
            library,
            min_library,
            max_library,
            analysis_type,
            objects,
        } => {
            out.push_str("set_operating_conditions");
            if let Some(at) = analysis_type {
                out.push_str(" -analysis_type ");
                out.push_str(at.as_str());
            }
            if let Some(lib) = library {
                out.push_str(" -library ");
                write_word(out, lib);
            }
            if let Some(m) = min {
                out.push_str(" -min ");
                write_word(out, m);
            }
            if let Some(m) = max {
                out.push_str(" -max ");
                write_word(out, m);
            }
            if let Some(ml) = min_library {
                out.push_str(" -min_library ");
                write_word(out, ml);
            }
            if let Some(ml) = max_library {
                out.push_str(" -max_library ");
                write_word(out, ml);
            }
            if let Some(objs) = objects {
                out.push_str(" -object_list ");
                write_obj(out, objs);
            }
            if let Some(c) = condition {
                out.push(' ');
                write_word(out, c);
            }
        }
        CmdKind::SetWireLoadModel {
            name,
            library,
            min,
            max,
            objects,
        } => {
            out.push_str("set_wire_load_model -name ");
            write_word(out, name);
            if let Some(lib) = library {
                out.push_str(" -library ");
                write_word(out, lib);
            }
            if *min {
                out.push_str(" -min");
            }
            if *max {
                out.push_str(" -max");
            }
            if let Some(objs) = objects {
                out.push(' ');
                write_obj(out, objs);
            }
        }
        CmdKind::SetWireLoadMode { mode } => {
            out.push_str("set_wire_load_mode ");
            out.push_str(mode.as_str());
        }
        CmdKind::SetWireLoadSelectionGroup {
            group_name,
            library,
            min,
            max,
            objects,
        } => {
            out.push_str("set_wire_load_selection_group");
            if let Some(lib) = library {
                out.push_str(" -library ");
                write_word(out, lib);
            }
            if *min {
                out.push_str(" -min");
            }
            if *max {
                out.push_str(" -max");
            }
            out.push(' ');
            write_word(out, group_name);
            if let Some(objs) = objects {
                out.push(' ');
                write_obj(out, objs);
            }
        }
        CmdKind::CreateClock {
            name,
            period,
            waveform,
            add,
            comment,
            sources,
        } => {
            out.push_str("create_clock");
            if let Some(n) = name {
                out.push_str(" -name ");
                write_word(out, n);
            }
            out.push_str(" -period ");
            write_f64(out, *period);
            if let Some([r, f]) = waveform {
                out.push_str(" -waveform {");
                write_f64(out, *r);
                out.push(' ');
                write_f64(out, *f);
                out.push('}');
            }
            if *add {
                out.push_str(" -add");
            }
            if let Some(c) = comment {
                out.push_str(" -comment ");
                write_word(out, c);
            }
            if let Some(src) = sources {
                out.push(' ');
                write_obj(out, src);
            }
        }
        CmdKind::CreateGeneratedClock {
            name,
            source,
            master_clock,
            divide_by,
            multiply_by,
            duty_cycle,
            edges,
            edge_shift,
            invert,
            combinational,
            add,
            comment,
            objects,
        } => {
            out.push_str("create_generated_clock");
            if let Some(n) = name {
                out.push_str(" -name ");
                write_word(out, n);
            }
            out.push_str(" -source ");
            write_obj(out, source);
            if let Some(mc) = master_clock {
                out.push_str(" -master_clock ");
                write_obj(out, mc);
            }
            if let Some(d) = divide_by {
                out.push_str(&format!(" -divide_by {d}"));
            }
            if let Some(m) = multiply_by {
                out.push_str(&format!(" -multiply_by {m}"));
            }
            if let Some(dc) = duty_cycle {
                out.push_str(" -duty_cycle ");
                write_f64(out, *dc);
            }
            if let Some([e0, e1, e2]) = edges {
                out.push_str(&format!(" -edges {{{e0} {e1} {e2}}}"));
            }
            if let Some([s0, s1, s2]) = edge_shift {
                out.push_str(" -edge_shift {");
                write_f64(out, *s0);
                out.push(' ');
                write_f64(out, *s1);
                out.push(' ');
                write_f64(out, *s2);
                out.push('}');
            }
            if *invert {
                out.push_str(" -invert");
            }
            if *combinational {
                out.push_str(" -combinational");
            }
            if *add {
                out.push_str(" -add");
            }
            if let Some(c) = comment {
                out.push_str(" -comment ");
                write_word(out, c);
            }
            out.push(' ');
            write_obj(out, objects);
        }
        CmdKind::SetPropagatedClock { objects } => {
            out.push_str("set_propagated_clock ");
            write_obj(out, objects);
        }
        CmdKind::UnsetPropagatedClock { objects } => {
            out.push_str("unset_propagated_clock ");
            write_obj(out, objects);
        }
        CmdKind::SetClockGroups {
            kind,
            name,
            allow_paths,
            comment,
            groups,
        } => {
            out.push_str("set_clock_groups");
            match kind {
                ClockGroupsKind::Asynchronous => out.push_str(" -asynchronous"),
                ClockGroupsKind::LogicallyExclusive => out.push_str(" -logically_exclusive"),
                ClockGroupsKind::PhysicallyExclusive => out.push_str(" -physically_exclusive"),
            }
            if let Some(n) = name {
                out.push_str(" -name ");
                write_word(out, n);
            }
            if *allow_paths {
                out.push_str(" -allow_paths");
            }
            for g in groups {
                out.push_str(" -group ");
                write_obj(out, g);
            }
            if let Some(c) = comment {
                out.push_str(" -comment ");
                write_word(out, c);
            }
        }
        CmdKind::SetClockUncertainty {
            uncertainty,
            setup_hold,
            from,
            to,
            objects,
        } => {
            out.push_str("set_clock_uncertainty");
            write_setup_hold_flags(out, *setup_hold);
            write_from_end(out, from);
            write_to_end(out, to);
            out.push(' ');
            write_f64(out, *uncertainty);
            if let Some(objs) = objects {
                out.push(' ');
                write_obj(out, objs);
            }
        }
        CmdKind::SetClockLatency {
            delay,
            source,
            edge,
            min_max,
            early_late,
            clock,
            objects,
        } => {
            out.push_str("set_clock_latency");
            if *source {
                out.push_str(" -source");
            }
            write_edge_flags(out, *edge);
            write_min_max_flags(out, *min_max);
            write_early_late_flags(out, *early_late);
            if let Some(clk) = clock {
                out.push_str(" -clock ");
                write_obj(out, clk);
            }
            out.push(' ');
            write_f64(out, *delay);
            out.push(' ');
            write_obj(out, objects);
        }
        CmdKind::SetClockTransition {
            transition,
            edge,
            min_max,
            clocks,
        } => {
            out.push_str("set_clock_transition");
            write_edge_flags(out, *edge);
            write_min_max_flags(out, *min_max);
            out.push(' ');
            write_f64(out, *transition);
            out.push(' ');
            write_obj(out, clocks);
        }
        CmdKind::SetInputDelay {
            delay,
            clock,
            clock_fall,
            level_sensitive,
            edge,
            min_max,
            add_delay,
            network_latency_included,
            source_latency_included,
            reference_pin,
            ports_or_pins,
        } => {
            out.push_str("set_input_delay");
            if let Some(clk) = clock {
                out.push_str(" -clock ");
                write_obj(out, clk);
            }
            if *clock_fall {
                out.push_str(" -clock_fall");
            }
            if *level_sensitive {
                out.push_str(" -level_sensitive");
            }
            write_edge_flags(out, *edge);
            write_min_max_flags(out, *min_max);
            if *add_delay {
                out.push_str(" -add_delay");
            }
            if *network_latency_included {
                out.push_str(" -network_latency_included");
            }
            if *source_latency_included {
                out.push_str(" -source_latency_included");
            }
            if let Some(rp) = reference_pin {
                out.push_str(" -reference_pin ");
                write_obj(out, rp);
            }
            out.push(' ');
            write_f64(out, *delay);
            out.push(' ');
            write_obj(out, ports_or_pins);
        }
        CmdKind::SetOutputDelay {
            delay,
            clock,
            clock_fall,
            level_sensitive,
            edge,
            min_max,
            add_delay,
            network_latency_included,
            source_latency_included,
            reference_pin,
            ports_or_pins,
        } => {
            out.push_str("set_output_delay");
            if let Some(clk) = clock {
                out.push_str(" -clock ");
                write_obj(out, clk);
            }
            if *clock_fall {
                out.push_str(" -clock_fall");
            }
            if *level_sensitive {
                out.push_str(" -level_sensitive");
            }
            write_edge_flags(out, *edge);
            write_min_max_flags(out, *min_max);
            if *add_delay {
                out.push_str(" -add_delay");
            }
            if *network_latency_included {
                out.push_str(" -network_latency_included");
            }
            if *source_latency_included {
                out.push_str(" -source_latency_included");
            }
            if let Some(rp) = reference_pin {
                out.push_str(" -reference_pin ");
                write_obj(out, rp);
            }
            out.push(' ');
            write_f64(out, *delay);
            out.push(' ');
            write_obj(out, ports_or_pins);
        }
        CmdKind::SetInputTransition {
            transition,
            edge,
            min_max,
            clock,
            clock_fall,
            ports,
        } => {
            out.push_str("set_input_transition");
            write_edge_flags(out, *edge);
            write_min_max_flags(out, *min_max);
            if let Some(clk) = clock {
                out.push_str(" -clock ");
                write_obj(out, clk);
            }
            if *clock_fall {
                out.push_str(" -clock_fall");
            }
            out.push(' ');
            write_f64(out, *transition);
            out.push(' ');
            write_obj(out, ports);
        }
        CmdKind::SetDrivingCell {
            lib_cell,
            library,
            pin,
            from_pin,
            edge,
            min_max,
            input_transition_rise,
            input_transition_fall,
            multiply_by,
            dont_scale,
            no_design_rule,
            clock,
            clock_fall,
            ports,
        } => {
            out.push_str("set_driving_cell -lib_cell ");
            write_word(out, lib_cell);
            if let Some(lib) = library {
                out.push_str(" -library ");
                write_word(out, lib);
            }
            if let Some(p) = pin {
                out.push_str(" -pin ");
                write_word(out, p);
            }
            if let Some(fp) = from_pin {
                out.push_str(" -from_pin ");
                write_word(out, fp);
            }
            write_edge_flags(out, *edge);
            write_min_max_flags(out, *min_max);
            if let Some(itr) = input_transition_rise {
                out.push_str(" -input_transition_rise ");
                write_f64(out, *itr);
            }
            if let Some(itf) = input_transition_fall {
                out.push_str(" -input_transition_fall ");
                write_f64(out, *itf);
            }
            if let Some(mb) = multiply_by {
                out.push_str(" -multiply_by ");
                write_f64(out, *mb);
            }
            if *dont_scale {
                out.push_str(" -dont_scale");
            }
            if *no_design_rule {
                out.push_str(" -no_design_rule");
            }
            if let Some(clk) = clock {
                out.push_str(" -clock ");
                write_obj(out, clk);
            }
            if *clock_fall {
                out.push_str(" -clock_fall");
            }
            out.push(' ');
            write_obj(out, ports);
        }
        CmdKind::SetLoad {
            value,
            pin_load,
            wire_load,
            subtract_pin_load,
            min_max,
            objects,
        } => {
            out.push_str("set_load");
            if *pin_load {
                out.push_str(" -pin_load");
            }
            if *wire_load {
                out.push_str(" -wire_load");
            }
            if *subtract_pin_load {
                out.push_str(" -subtract_pin_load");
            }
            write_min_max_flags(out, *min_max);
            out.push(' ');
            write_f64(out, *value);
            out.push(' ');
            write_obj(out, objects);
        }
        CmdKind::SetFalsePath {
            setup_hold,
            edge,
            from,
            through,
            to,
            comment,
        } => {
            out.push_str("set_false_path");
            write_setup_hold_flags(out, *setup_hold);
            write_edge_flags(out, *edge);
            write_from_end(out, from);
            write_through_ends(out, through);
            write_to_end(out, to);
            if let Some(c) = comment {
                out.push_str(" -comment ");
                write_word(out, c);
            }
        }
        CmdKind::SetMulticyclePath {
            multiplier,
            setup_hold,
            start_end,
            edge,
            from,
            through,
            to,
            comment,
        } => {
            out.push_str("set_multicycle_path");
            write_setup_hold_flags(out, *setup_hold);
            write_start_end_flags(out, *start_end);
            write_edge_flags(out, *edge);
            write_from_end(out, from);
            write_through_ends(out, through);
            write_to_end(out, to);
            if let Some(c) = comment {
                out.push_str(" -comment ");
                write_word(out, c);
            }
            out.push_str(&format!(" {multiplier}"));
        }
        CmdKind::SetMaxDelay {
            delay,
            edge,
            from,
            through,
            to,
            ignore_clock_latency,
            comment,
        } => {
            out.push_str("set_max_delay");
            write_edge_flags(out, *edge);
            if *ignore_clock_latency {
                out.push_str(" -ignore_clock_latency");
            }
            write_from_end(out, from);
            write_through_ends(out, through);
            write_to_end(out, to);
            if let Some(c) = comment {
                out.push_str(" -comment ");
                write_word(out, c);
            }
            out.push(' ');
            write_f64(out, *delay);
        }
        CmdKind::SetMinDelay {
            delay,
            edge,
            from,
            through,
            to,
            ignore_clock_latency,
            comment,
        } => {
            out.push_str("set_min_delay");
            write_edge_flags(out, *edge);
            if *ignore_clock_latency {
                out.push_str(" -ignore_clock_latency");
            }
            write_from_end(out, from);
            write_through_ends(out, through);
            write_to_end(out, to);
            if let Some(c) = comment {
                out.push_str(" -comment ");
                write_word(out, c);
            }
            out.push(' ');
            write_f64(out, *delay);
        }
        CmdKind::SetCaseAnalysis {
            value,
            ports_or_pins,
        } => {
            out.push_str("set_case_analysis ");
            out.push_str(value.as_str());
            out.push(' ');
            write_obj(out, ports_or_pins);
        }
        CmdKind::SetLogicZero { ports } => {
            out.push_str("set_logic_zero ");
            write_obj(out, ports);
        }
        CmdKind::SetLogicOne { ports } => {
            out.push_str("set_logic_one ");
            write_obj(out, ports);
        }
        CmdKind::SetDisableTiming { from, to, objects } => {
            out.push_str("set_disable_timing");
            if let Some(f) = from {
                out.push_str(" -from ");
                write_word(out, f);
            }
            if let Some(t) = to {
                out.push_str(" -to ");
                write_word(out, t);
            }
            out.push(' ');
            write_obj(out, objects);
        }
        CmdKind::SetMaxTimeBorrow { value, objects } => {
            out.push_str("set_max_time_borrow ");
            write_f64(out, *value);
            out.push(' ');
            write_obj(out, objects);
        }
        CmdKind::SetTimingDerate {
            factor,
            early_late,
            edge,
            clock,
            data,
            net_delay,
            cell_delay,
            cell_check,
            static_derate,
            dynamic,
            increment,
            objects,
        } => {
            out.push_str("set_timing_derate");
            write_early_late_flags(out, *early_late);
            write_edge_flags(out, *edge);
            if *clock {
                out.push_str(" -clock");
            }
            if *data {
                out.push_str(" -data");
            }
            if *net_delay {
                out.push_str(" -net_delay");
            }
            if *cell_delay {
                out.push_str(" -cell_delay");
            }
            if *cell_check {
                out.push_str(" -cell_check");
            }
            if *static_derate {
                out.push_str(" -static");
            }
            if *dynamic {
                out.push_str(" -dynamic");
            }
            if *increment {
                out.push_str(" -increment");
            }
            out.push(' ');
            write_f64(out, *factor);
            if let Some(objs) = objects {
                out.push(' ');
                write_obj(out, objs);
            }
        }
        CmdKind::SetMinPulseWidth {
            value,
            high,
            low,
            objects,
        } => {
            out.push_str("set_min_pulse_width");
            if *high {
                out.push_str(" -high");
            }
            if *low {
                out.push_str(" -low");
            }
            out.push(' ');
            write_f64(out, *value);
            out.push(' ');
            write_obj(out, objects);
        }
        CmdKind::SetIdealNetwork {
            no_propagate,
            objects,
        } => {
            out.push_str("set_ideal_network");
            if *no_propagate {
                out.push_str(" -no_propagate");
            }
            out.push(' ');
            write_obj(out, objects);
        }
        CmdKind::GroupPath {
            name,
            default,
            weight,
            critical_range,
            from,
            through,
            to,
            comment,
        } => {
            out.push_str("group_path");
            if let Some(n) = name {
                out.push_str(" -name ");
                write_word(out, n);
            }
            if *default {
                out.push_str(" -default");
            }
            if let Some(w) = weight {
                out.push_str(" -weight ");
                write_f64(out, *w);
            }
            if let Some(cr) = critical_range {
                out.push_str(" -critical_range ");
                write_f64(out, *cr);
            }
            write_from_end(out, from);
            write_through_ends(out, through);
            write_to_end(out, to);
            if let Some(c) = comment {
                out.push_str(" -comment ");
                write_word(out, c);
            }
        }
        CmdKind::SetMaxTransition {
            value,
            clock_path,
            data_path,
            edge,
            objects,
        } => {
            out.push_str("set_max_transition");
            if *clock_path {
                out.push_str(" -clock_path");
            }
            if *data_path {
                out.push_str(" -data_path");
            }
            write_edge_flags(out, *edge);
            out.push(' ');
            write_f64(out, *value);
            out.push(' ');
            write_obj(out, objects);
        }
        CmdKind::SetMaxCapacitance {
            value,
            clock_path,
            data_path,
            edge,
            objects,
        } => {
            out.push_str("set_max_capacitance");
            if *clock_path {
                out.push_str(" -clock_path");
            }
            if *data_path {
                out.push_str(" -data_path");
            }
            write_edge_flags(out, *edge);
            out.push(' ');
            write_f64(out, *value);
            out.push(' ');
            write_obj(out, objects);
        }
        CmdKind::SetMinCapacitance {
            value,
            clock_path,
            data_path,
            edge,
            objects,
        } => {
            out.push_str("set_min_capacitance");
            if *clock_path {
                out.push_str(" -clock_path");
            }
            if *data_path {
                out.push_str(" -data_path");
            }
            write_edge_flags(out, *edge);
            out.push(' ');
            write_f64(out, *value);
            out.push(' ');
            write_obj(out, objects);
        }
        CmdKind::SetMaxFanout { value, objects } => {
            out.push_str("set_max_fanout ");
            write_f64(out, *value);
            out.push(' ');
            write_obj(out, objects);
        }
        CmdKind::SetFanoutLoad { value, ports } => {
            out.push_str("set_fanout_load ");
            write_f64(out, *value);
            out.push(' ');
            write_obj(out, ports);
        }
        CmdKind::SetPortFanoutNumber { value, ports } => {
            out.push_str(&format!("set_port_fanout_number {value} "));
            write_obj(out, ports);
        }
        CmdKind::SetDontUse { lib_cells } => {
            out.push_str("set_dont_use ");
            write_obj(out, lib_cells);
        }
        CmdKind::SetDontTouch { objects } => {
            out.push_str("set_dont_touch ");
            write_obj(out, objects);
        }
        CmdKind::SetDontTouchNetwork {
            no_propagate,
            objects,
        } => {
            out.push_str("set_dont_touch_network");
            if *no_propagate {
                out.push_str(" -no_propagate");
            }
            out.push(' ');
            write_obj(out, objects);
        }
        CmdKind::SetSizeOnly {
            instances,
            no_instances,
        } => {
            out.push_str("set_size_only");
            if *no_instances {
                out.push_str(" -no_instances");
            }
            out.push(' ');
            write_obj(out, instances);
        }
        CmdKind::SetMaxArea { value, ignore_tns } => {
            out.push_str("set_max_area");
            if *ignore_tns {
                out.push_str(" -ignore_tns");
            }
            out.push(' ');
            write_f64(out, *value);
        }
        CmdKind::SetMaxDynamicPower { value, unit } => {
            out.push_str("set_max_dynamic_power");
            if let Some(u) = unit {
                out.push_str(" -unit ");
                write_word(out, u);
            }
            out.push(' ');
            write_f64(out, *value);
        }
        CmdKind::SetMaxLeakagePower { value, unit } => {
            out.push_str("set_max_leakage_power");
            if let Some(u) = unit {
                out.push_str(" -unit ");
                write_word(out, u);
            }
            out.push(' ');
            write_f64(out, *value);
        }
    }
}
