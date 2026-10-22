// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! `CONSTR v1` section decoder and validator (`SDC-REQ-0049`, `SDC-REQ-0050`).

use rusteda_core::diag::{Diag, Location, Severity};

use super::super::cmd::{
    AnalysisType, CaseValue, ClockGroupsKind, Cmd, CmdKind, ExceptionEnd, WireLoadMode,
};
use super::super::filter::Filter;
use super::super::obj::{Obj, ObjQuery};
use super::super::{EnvEntry, Sdc, SourceRecord, Units};
use super::encode::{
    decode_early_late, decode_edge, decode_min_max, decode_setup_hold, decode_start_end,
};
use super::*;

/// Decodes and validates a slice of `CONSTR v1` sections into [`Sdc`] (SDC-REQ-0049,
/// SDC-REQ-0050).
pub fn decode_sections(sections: &[ConstrSection]) -> Result<Sdc, ConstrError> {
    let find_sec = |id: u16, elem_size: u32| -> Result<&[u8], ConstrError> {
        let mut matches = sections.iter().filter(|s| s.id == id);
        let s = matches
            .next()
            .ok_or_else(|| ConstrError(format!("missing CONSTR section id {id}")))?;
        if matches.next().is_some() {
            return Err(ConstrError(format!("duplicate CONSTR section id {id}")));
        }
        if s.elem_size != elem_size {
            return Err(ConstrError(format!(
                "section {id} elem_size {}, expected {elem_size}",
                s.elem_size
            )));
        }
        let expected_bytes = (s.count as usize)
            .checked_mul(elem_size as usize)
            .ok_or_else(|| ConstrError(format!("section {id} byte length overflow")))?;
        if s.bytes.len() != expected_bytes {
            return Err(ConstrError(format!(
                "section {id} byte length {} != count * elem_size ({expected_bytes})",
                s.bytes.len()
            )));
        }
        Ok(&s.bytes)
    };

    let str_bytes = find_sec(section_id::STR_BYTES, 1)?;
    let str_offs_b = find_sec(section_id::STR_OFFS, 4)?;
    let header_b = find_sec(section_id::HEADER, 32)?;
    let sources_b = find_sec(section_id::SOURCES, 16)?;
    let env_b = find_sec(section_id::ENV, 8)?;
    let cmds_b = find_sec(section_id::CMDS, 24)?;
    let args_b = find_sec(section_id::ARGS, 16)?;
    let f64_b = find_sec(section_id::F64, 8)?;
    let objs_b = find_sec(section_id::OBJS, 16)?;
    let obj_child_b = find_sec(section_id::OBJ_CHILD, 4)?;
    let pats_b = find_sec(section_id::PATS, 4)?;
    let canon_b = find_sec(section_id::CANON, 1)?;
    let diags_b = find_sec(section_id::DIAGS, 24)?;

    if header_b.len() != 32 {
        return Err(ConstrError("HEADER section must have count 1".to_string()));
    }
    std::str::from_utf8(canon_b)
        .map_err(|_| ConstrError("CANON section is not valid UTF-8".to_string()))?;
    let str_pool_str = std::str::from_utf8(str_bytes)
        .map_err(|_| ConstrError("STR_BYTES section is not valid UTF-8".to_string()))?;

    let n_offs = str_offs_b.len() / 4;
    if n_offs < 1 {
        return Err(ConstrError(
            "STR_OFFS must have at least 1 offset".to_string(),
        ));
    }
    let mut offs = Vec::with_capacity(n_offs);
    for i in 0..n_offs {
        let off = read_u32(str_offs_b, i * 4);
        if i == 0 && off != 0 {
            return Err(ConstrError("STR_OFFS[0] must be 0".to_string()));
        }
        if let Some(&prev) = offs.last()
            && off < prev
        {
            return Err(ConstrError("STR_OFFS is not monotonic".to_string()));
        }
        offs.push(off);
    }
    if *offs.last().unwrap() as usize != str_bytes.len() {
        return Err(ConstrError(
            "STR_OFFS last offset does not match STR_BYTES length".to_string(),
        ));
    }
    let get_str = |id: u32| -> Result<String, ConstrError> {
        let idx = id as usize;
        if idx + 1 >= offs.len() {
            return Err(ConstrError(format!("string id {id} out of bounds")));
        }
        let s_off = offs[idx] as usize;
        let e_off = offs[idx + 1] as usize;
        str_pool_str
            .get(s_off..e_off)
            .map(str::to_string)
            .ok_or_else(|| ConstrError(format!("string id {id} slice out of UTF-8 boundary")))
    };
    let get_opt_str = |id: u32| -> Result<Option<String>, ConstrError> {
        if id == NONE_U32 {
            Ok(None)
        } else {
            get_str(id).map(Some)
        }
    };

    // Decode HEADER
    let time_f = f64::from_bits(read_u64(header_b, 0));
    let cap_f = f64::from_bits(read_u64(header_b, 8));
    let res_f = f64::from_bits(read_u64(header_b, 16));
    let cur_design_id = read_u32(header_b, 24);
    let header_flags = read_u32(header_b, 28);
    let current_design = get_opt_str(cur_design_id)?;
    let units = Units {
        time_to_ns: if time_f.is_nan() { None } else { Some(time_f) },
        cap_to_pf: if cap_f.is_nan() { None } else { Some(cap_f) },
        res_to_kohm: if res_f.is_nan() { None } else { Some(res_f) },
        voltage_to_v: if header_flags & (1 << 0) != 0 {
            Some(1.0)
        } else {
            None
        },
        current_to_ma: if header_flags & (1 << 1) != 0 {
            Some(1.0)
        } else {
            None
        },
        power_to_nw: if header_flags & (1 << 2) != 0 {
            Some(1.0)
        } else {
            None
        },
    };

    // Decode SOURCES
    let n_sources = sources_b.len() / 16;
    let mut sources = Vec::with_capacity(n_sources);
    for i in 0..n_sources {
        let base = i * 16;
        let path = get_str(read_u32(sources_b, base))?;
        let flags = read_u32(sources_b, base + 4);
        let xxh3 = read_u64(sources_b, base + 8);
        sources.push(SourceRecord { path, flags, xxh3 });
    }

    // Decode ENV
    let n_env = env_b.len() / 8;
    let mut env = Vec::with_capacity(n_env);
    for i in 0..n_env {
        let base = i * 8;
        let name_word = read_u32(env_b, base);
        let written = (name_word >> 31) != 0;
        let name = get_str(name_word & 0x7FFF_FFFF)?;
        let value = get_str(read_u32(env_b, base + 4))?;
        env.push(EnvEntry {
            name,
            value,
            written,
        });
    }

    // Decode PATS, OBJ_CHILD, F64
    let n_pats = pats_b.len() / 4;
    let mut pats = Vec::with_capacity(n_pats);
    for i in 0..n_pats {
        pats.push(get_str(read_u32(pats_b, i * 4))?);
    }
    let n_child = obj_child_b.len() / 4;
    let mut obj_children = Vec::with_capacity(n_child);
    for i in 0..n_child {
        obj_children.push(read_u32(obj_child_b, i * 4));
    }
    let n_f64 = f64_b.len() / 8;
    let mut f64_vals = Vec::with_capacity(n_f64);
    for i in 0..n_f64 {
        f64_vals.push(f64::from_bits(read_u64(f64_b, i * 8)));
    }

    // Decode OBJS (post-order encoded, so children have smaller index than parent, or decode with recursion guard)
    let n_objs = objs_b.len() / 16;
    let mut decoded_objs: Vec<Option<Obj>> = vec![None; n_objs];
    for idx in 0..n_objs {
        let obj = decode_obj_at(
            idx,
            objs_b,
            &decoded_objs,
            &pats,
            &obj_children,
            &get_opt_str,
        )?;
        decoded_objs[idx] = Some(obj);
    }
    let get_obj = |id: u32| -> Result<Obj, ConstrError> {
        decoded_objs
            .get(id as usize)
            .and_then(Clone::clone)
            .ok_or_else(|| ConstrError(format!("obj id {id} out of bounds")))
    };

    // Decode ARGS
    let n_args = args_b.len() / 16;
    let mut raw_args = Vec::with_capacity(n_args);
    for i in 0..n_args {
        let base = i * 16;
        let key = read_u16(args_b, base);
        let kind = args_b[base + 2];
        let a = read_u32(args_b, base + 4);
        let b = read_u64(args_b, base + 8);
        raw_args.push(RawArg { key, kind, a, b });
    }

    // Decode CMDS
    let n_cmds = cmds_b.len() / 24;
    let mut cmds = Vec::with_capacity(n_cmds);
    for i in 0..n_cmds {
        let base = i * 24;
        let op = read_u16(cmds_b, base);
        let src = read_u16(cmds_b, base + 2);
        let flags = read_u32(cmds_b, base + 4);
        let line = read_u32(cmds_b, base + 8);
        let col = read_u32(cmds_b, base + 12);
        let arg_start = read_u32(cmds_b, base + 16) as usize;
        let arg_count = read_u32(cmds_b, base + 20) as usize;
        let arg_end = arg_start
            .checked_add(arg_count)
            .ok_or_else(|| ConstrError("cmd arg range overflow".to_string()))?;
        if arg_end > raw_args.len() {
            return Err(ConstrError("cmd arg range out of bounds".to_string()));
        }
        let slice = &raw_args[arg_start..arg_end];
        let kind = decode_cmd_kind(op, flags, slice, &get_str, &get_obj, &f64_vals)?;
        cmds.push(Cmd {
            kind,
            source: src,
            line,
            col,
        });
    }

    // Decode DIAGS
    let n_diags = diags_b.len() / 24;
    let mut diags = Vec::with_capacity(n_diags);
    for i in 0..n_diags {
        let base = i * 24;
        let word0 = read_u32(diags_b, base);
        let sev = match word0 & 0x3 {
            0 => Severity::Note,
            1 => Severity::Warning,
            2 => Severity::Error,
            other => return Err(ConstrError(format!("invalid diag severity {other}"))),
        };
        let code = get_str(word0 >> 2)?;
        let file_id = read_u32(diags_b, base + 4);
        let line = read_u32(diags_b, base + 8);
        let col = read_u32(diags_b, base + 12);
        let message = get_str(read_u32(diags_b, base + 16))?;
        let help = get_opt_str(read_u32(diags_b, base + 20))?;
        let location = if file_id == NONE_U32 {
            None
        } else {
            Some(Location {
                file: get_str(file_id)?,
                line,
                column: col,
            })
        };
        diags.push(Diag {
            severity: sev,
            code,
            location,
            message,
            help,
        });
    }

    Ok(Sdc {
        units,
        current_design,
        sources,
        env,
        cmds,
        diags,
    })
}

fn decode_obj_at(
    idx: usize,
    objs_b: &[u8],
    decoded: &[Option<Obj>],
    pats: &[String],
    obj_children: &[u32],
    get_opt_str: &dyn Fn(u32) -> Result<Option<String>, ConstrError>,
) -> Result<Obj, ConstrError> {
    let base = idx * 16;
    let kind = objs_b[base];
    let flags = objs_b[base + 1];
    let pad = read_u16(objs_b, base + 2);
    let a = read_u32(objs_b, base + 4);
    let b = read_u32(objs_b, base + 8);
    let filter_id = read_u32(objs_b, base + 12);

    let get_prev_obj = |id: u32| -> Result<Obj, ConstrError> {
        let i = id as usize;
        if i >= idx {
            return Err(ConstrError(format!(
                "invalid forward/cycle obj reference {id} from {idx}"
            )));
        }
        decoded[i]
            .clone()
            .ok_or_else(|| ConstrError(format!("missing decoded obj {id}")))
    };

    let get_pat_slice = |start: u32, len: u32| -> Result<Vec<String>, ConstrError> {
        let s = start as usize;
        let e = s
            .checked_add(len as usize)
            .ok_or_else(|| ConstrError("pattern slice overflow".to_string()))?;
        pats.get(s..e)
            .map(<[String]>::to_vec)
            .ok_or_else(|| ConstrError("pattern slice out of bounds".to_string()))
    };

    match kind {
        OBJ_PORTS | OBJ_PINS | OBJ_CELLS | OBJ_NETS | OBJ_CLOCKS | OBJ_LIB_CELLS | OBJ_LIB_PINS => {
            let patterns = get_pat_slice(a, b)?;
            let of_objects = if flags & OBJ_FLAG_OF_OBJECTS != 0 {
                let slot = pad as usize;
                let &child_id = obj_children.get(slot).ok_or_else(|| {
                    ConstrError("of_objects child slot out of bounds".to_string())
                })?;
                Some(Box::new(get_prev_obj(child_id)?))
            } else {
                None
            };
            let filter = match get_opt_str(filter_id)? {
                Some(s) => Some(
                    Filter::parse(&s)
                        .map_err(|e| ConstrError(format!("invalid filter in OBJS: {e}")))?,
                ),
                None => None,
            };
            let q = ObjQuery {
                patterns,
                hierarchical: flags & OBJ_FLAG_HIER != 0,
                regexp: flags & OBJ_FLAG_REGEXP != 0,
                nocase: flags & OBJ_FLAG_NOCASE != 0,
                of_objects,
                filter,
                quiet: flags & OBJ_FLAG_QUIET != 0,
            };
            Ok(match kind {
                OBJ_PORTS => Obj::Ports(q),
                OBJ_PINS => Obj::Pins(q),
                OBJ_CELLS => Obj::Cells(q),
                OBJ_NETS => Obj::Nets(q),
                OBJ_CLOCKS => Obj::Clocks(q),
                OBJ_LIB_CELLS => Obj::LibCells(q),
                OBJ_LIB_PINS => Obj::LibPins(q),
                _ => unreachable!(),
            })
        }
        OBJ_ALL_INPUTS => Ok(Obj::AllInputs {
            no_clocks: flags & OBJ_FLAG_NO_CLOCKS != 0,
        }),
        OBJ_ALL_OUTPUTS => Ok(Obj::AllOutputs),
        OBJ_ALL_CLOCKS => Ok(Obj::AllClocks),
        OBJ_ALL_REGISTERS => Ok(Obj::AllRegisters),
        OBJ_CURRENT_DESIGN => Ok(Obj::CurrentDesign),
        OBJ_NAMES => Ok(Obj::Names(get_pat_slice(a, b)?)),
        OBJ_UNION => {
            let s = a as usize;
            let e = s
                .checked_add(b as usize)
                .ok_or_else(|| ConstrError("OBJ_CHILD slice overflow".to_string()))?;
            let child_ids = obj_children
                .get(s..e)
                .ok_or_else(|| ConstrError("OBJ_CHILD slice out of bounds".to_string()))?;
            let mut items = Vec::with_capacity(child_ids.len());
            for &cid in child_ids {
                items.push(get_prev_obj(cid)?);
            }
            Ok(Obj::Union(items))
        }
        OBJ_DIFF => {
            let lhs = get_prev_obj(a)?;
            let rhs = get_prev_obj(b)?;
            Ok(Obj::Diff(Box::new(lhs), Box::new(rhs)))
        }
        other => Err(ConstrError(format!("unknown OBJS kind {other}"))),
    }
}

#[derive(Clone, Copy)]
struct RawArg {
    key: u16,
    kind: u8,
    a: u32,
    b: u64,
}

fn decode_cmd_kind(
    op: u16,
    flags: u32,
    args: &[RawArg],
    get_str: &dyn Fn(u32) -> Result<String, ConstrError>,
    get_obj: &dyn Fn(u32) -> Result<Obj, ConstrError>,
    f64_vals: &[f64],
) -> Result<CmdKind, ConstrError> {
    let find_str = |key: u16| -> Result<Option<String>, ConstrError> {
        for a in args {
            if a.key == key && a.kind == ARG_STR {
                return get_str(a.b as u32).map(Some);
            }
        }
        Ok(None)
    };
    let req_str = |key: u16| -> Result<String, ConstrError> {
        find_str(key)?.ok_or_else(|| ConstrError(format!("missing required string arg key {key}")))
    };
    let find_num = |key: u16| -> Option<f64> {
        args.iter()
            .find(|a| a.key == key && a.kind == ARG_NUM)
            .map(|a| f64::from_bits(a.b))
    };
    let req_num = |key: u16| -> Result<f64, ConstrError> {
        find_num(key).ok_or_else(|| ConstrError(format!("missing required num arg key {key}")))
    };
    let find_int = |key: u16| -> Option<u32> {
        args.iter()
            .find(|a| a.key == key && a.kind == ARG_INT)
            .map(|a| a.b as u32)
    };
    let req_int = |key: u16| -> Result<u32, ConstrError> {
        find_int(key).ok_or_else(|| ConstrError(format!("missing required int arg key {key}")))
    };
    let find_enum = |key: u16| -> Option<u32> {
        args.iter()
            .find(|a| a.key == key && a.kind == ARG_ENUM)
            .map(|a| a.b as u32)
    };
    let find_obj = |key: u16| -> Result<Option<Obj>, ConstrError> {
        for a in args {
            if a.key == key && a.kind == ARG_OBJ {
                return get_obj(a.b as u32).map(Some);
            }
        }
        Ok(None)
    };
    let req_obj = |key: u16| -> Result<Obj, ConstrError> {
        find_obj(key)?.ok_or_else(|| ConstrError(format!("missing required obj arg key {key}")))
    };
    let find_end = |key: u16| -> Result<Option<ExceptionEnd>, ConstrError> {
        for a in args {
            if a.key == key && a.kind == ARG_OBJ {
                let obj = get_obj(a.b as u32)?;
                let edge = decode_edge(a.a)?;
                return Ok(Some(ExceptionEnd { obj, edge }));
            }
        }
        Ok(None)
    };
    let find_ends = |key: u16| -> Result<Vec<ExceptionEnd>, ConstrError> {
        let mut out = Vec::new();
        for a in args {
            if a.key == key && a.kind == ARG_OBJ {
                let obj = get_obj(a.b as u32)?;
                let edge = decode_edge(a.a)?;
                out.push(ExceptionEnd { obj, edge });
            }
        }
        Ok(out)
    };
    let find_f64_list = |key: u16| -> Result<Option<Vec<f64>>, ConstrError> {
        for a in args {
            if a.key == key && a.kind == ARG_F64_LIST {
                let s = a.a as usize;
                let len = a.b as usize;
                let e = s
                    .checked_add(len)
                    .ok_or_else(|| ConstrError("F64 slice overflow".to_string()))?;
                let slice = f64_vals
                    .get(s..e)
                    .ok_or_else(|| ConstrError("F64 slice out of bounds".to_string()))?;
                return Ok(Some(slice.to_vec()));
            }
        }
        Ok(None)
    };

    match op {
        OP_SET_UNITS => Ok(CmdKind::SetUnits {
            time: find_str(KEY_UNIT_TIME)?,
            capacitance: find_str(KEY_UNIT_CAP)?,
            resistance: find_str(KEY_UNIT_RES)?,
            voltage: find_str(KEY_UNIT_VOLT)?,
            current: find_str(KEY_UNIT_CURR)?,
            power: find_str(KEY_UNIT_POWER)?,
        }),
        OP_CURRENT_DESIGN => Ok(CmdKind::CurrentDesign {
            name: req_str(KEY_POS0)?,
        }),
        OP_SET_OP_COND => {
            let analysis_type = match find_enum(KEY_ANALYSIS_TYPE) {
                Some(0) => Some(AnalysisType::Single),
                Some(1) => Some(AnalysisType::BcWc),
                Some(2) => Some(AnalysisType::OnChipVariation),
                Some(other) => return Err(ConstrError(format!("invalid analysis_type {other}"))),
                None => None,
            };
            Ok(CmdKind::SetOperatingConditions {
                condition: find_str(KEY_POS0)?,
                min: find_str(KEY_MIN)?,
                max: find_str(KEY_MAX)?,
                library: find_str(KEY_LIBRARY)?,
                min_library: find_str(KEY_MIN_LIB)?,
                max_library: find_str(KEY_MAX_LIB)?,
                analysis_type,
                objects: find_obj(KEY_OBJECTS)?,
            })
        }
        OP_SET_WLM => Ok(CmdKind::SetWireLoadModel {
            name: req_str(KEY_NAME)?,
            library: find_str(KEY_LIBRARY)?,
            min: flags & (1 << 0) != 0,
            max: flags & (1 << 1) != 0,
            objects: find_obj(KEY_OBJECTS)?,
        }),
        OP_SET_WL_MODE => {
            let mode = match find_enum(KEY_POS0) {
                Some(0) => WireLoadMode::Top,
                Some(1) => WireLoadMode::Enclosed,
                Some(2) => WireLoadMode::Segmented,
                other => return Err(ConstrError(format!("invalid wire_load_mode {other:?}"))),
            };
            Ok(CmdKind::SetWireLoadMode { mode })
        }
        OP_SET_WLSG => Ok(CmdKind::SetWireLoadSelectionGroup {
            group_name: req_str(KEY_POS0)?,
            library: find_str(KEY_LIBRARY)?,
            min: flags & (1 << 0) != 0,
            max: flags & (1 << 1) != 0,
            objects: find_obj(KEY_OBJECTS)?,
        }),
        OP_CREATE_CLOCK => {
            let waveform = match find_f64_list(KEY_WAVEFORM)? {
                Some(v) => {
                    if v.len() != 2 {
                        return Err(ConstrError("waveform must have 2 elements".to_string()));
                    }
                    Some([v[0], v[1]])
                }
                None => None,
            };
            Ok(CmdKind::CreateClock {
                name: find_str(KEY_NAME)?,
                period: req_num(KEY_PERIOD)?,
                waveform,
                add: flags & (1 << 0) != 0,
                comment: find_str(KEY_COMMENT)?,
                sources: find_obj(KEY_OBJECTS)?,
            })
        }
        OP_CREATE_GEN_CLOCK => {
            let edges = args
                .iter()
                .find(|a| a.key == KEY_EDGES && a.kind == ARG_INT_TRIPLE)
                .map(|a| [a.a, a.b as u32, (a.b >> 32) as u32]);
            let edge_shift = match find_f64_list(KEY_EDGE_SHIFT)? {
                Some(v) => {
                    if v.len() != 3 {
                        return Err(ConstrError("edge_shift must have 3 elements".to_string()));
                    }
                    Some([v[0], v[1], v[2]])
                }
                None => None,
            };
            Ok(CmdKind::CreateGeneratedClock {
                name: find_str(KEY_NAME)?,
                source: req_obj(KEY_SOURCE)?,
                master_clock: find_obj(KEY_MASTER_CLOCK)?,
                divide_by: find_int(KEY_DIVIDE_BY),
                multiply_by: find_int(KEY_MULTIPLY_BY),
                duty_cycle: find_num(KEY_DUTY_CYCLE),
                edges,
                edge_shift,
                invert: flags & (1 << 0) != 0,
                combinational: flags & (1 << 1) != 0,
                add: flags & (1 << 2) != 0,
                comment: find_str(KEY_COMMENT)?,
                objects: req_obj(KEY_OBJECTS)?,
            })
        }
        OP_SET_PROP_CLOCK => Ok(CmdKind::SetPropagatedClock {
            objects: req_obj(KEY_OBJECTS)?,
        }),
        OP_UNSET_PROP_CLOCK => Ok(CmdKind::UnsetPropagatedClock {
            objects: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_CLOCK_GROUPS => {
            let kind = match flags & 0x3 {
                0 => ClockGroupsKind::Asynchronous,
                1 => ClockGroupsKind::LogicallyExclusive,
                2 => ClockGroupsKind::PhysicallyExclusive,
                other => return Err(ConstrError(format!("invalid clock_groups kind {other}"))),
            };
            let mut groups = Vec::new();
            for a in args {
                if a.key == KEY_GROUP && a.kind == ARG_OBJ {
                    groups.push(get_obj(a.b as u32)?);
                }
            }
            Ok(CmdKind::SetClockGroups {
                kind,
                name: find_str(KEY_NAME)?,
                allow_paths: flags & (1 << 2) != 0,
                comment: find_str(KEY_COMMENT)?,
                groups,
            })
        }
        OP_SET_CLOCK_UNCERTAINTY => Ok(CmdKind::SetClockUncertainty {
            uncertainty: req_num(KEY_POS0)?,
            setup_hold: decode_setup_hold(flags)?,
            from: find_end(KEY_FROM)?,
            to: find_end(KEY_TO)?,
            objects: find_obj(KEY_OBJECTS)?,
        }),
        OP_SET_CLOCK_LATENCY => Ok(CmdKind::SetClockLatency {
            delay: req_num(KEY_POS0)?,
            source: flags & (1 << 0) != 0,
            edge: decode_edge(flags >> 1)?,
            min_max: decode_min_max(flags >> 3)?,
            early_late: decode_early_late(flags >> 5)?,
            clock: find_obj(KEY_CLOCK)?,
            objects: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_CLOCK_TRANSITION => Ok(CmdKind::SetClockTransition {
            transition: req_num(KEY_POS0)?,
            edge: decode_edge(flags)?,
            min_max: decode_min_max(flags >> 2)?,
            clocks: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_INPUT_DELAY | OP_SET_OUTPUT_DELAY => {
            let delay = req_num(KEY_POS0)?;
            let clock = find_obj(KEY_CLOCK)?;
            let clock_fall = flags & (1 << 0) != 0;
            let level_sensitive = flags & (1 << 1) != 0;
            let edge = decode_edge(flags >> 2)?;
            let min_max = decode_min_max(flags >> 4)?;
            let add_delay = flags & (1 << 6) != 0;
            let network_latency_included = flags & (1 << 7) != 0;
            let source_latency_included = flags & (1 << 8) != 0;
            let reference_pin = find_obj(KEY_REFERENCE_PIN)?;
            let ports_or_pins = req_obj(KEY_OBJECTS)?;
            if op == OP_SET_INPUT_DELAY {
                Ok(CmdKind::SetInputDelay {
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
                })
            } else {
                Ok(CmdKind::SetOutputDelay {
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
                })
            }
        }
        OP_SET_INPUT_TRANSITION => Ok(CmdKind::SetInputTransition {
            transition: req_num(KEY_POS0)?,
            edge: decode_edge(flags)?,
            min_max: decode_min_max(flags >> 2)?,
            clock: find_obj(KEY_CLOCK)?,
            clock_fall: flags & (1 << 4) != 0,
            ports: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_DRIVING_CELL => Ok(CmdKind::SetDrivingCell {
            lib_cell: req_str(KEY_LIB_CELL)?,
            library: find_str(KEY_LIBRARY)?,
            pin: find_str(KEY_PIN)?,
            from_pin: find_str(KEY_FROM_PIN)?,
            edge: decode_edge(flags)?,
            min_max: decode_min_max(flags >> 2)?,
            input_transition_rise: find_num(KEY_IN_TRANS_RISE),
            input_transition_fall: find_num(KEY_IN_TRANS_FALL),
            multiply_by: find_num(KEY_MULTIPLY_BY),
            dont_scale: flags & (1 << 4) != 0,
            no_design_rule: flags & (1 << 5) != 0,
            clock: find_obj(KEY_CLOCK)?,
            clock_fall: flags & (1 << 6) != 0,
            ports: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_LOAD => Ok(CmdKind::SetLoad {
            value: req_num(KEY_POS0)?,
            pin_load: flags & (1 << 0) != 0,
            wire_load: flags & (1 << 1) != 0,
            subtract_pin_load: flags & (1 << 2) != 0,
            min_max: decode_min_max(flags >> 3)?,
            objects: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_FALSE_PATH => Ok(CmdKind::SetFalsePath {
            setup_hold: decode_setup_hold(flags)?,
            edge: decode_edge(flags >> 2)?,
            from: find_end(KEY_FROM)?,
            through: find_ends(KEY_THROUGH)?,
            to: find_end(KEY_TO)?,
            comment: find_str(KEY_COMMENT)?,
        }),
        OP_SET_MULTICYCLE_PATH => Ok(CmdKind::SetMulticyclePath {
            multiplier: req_int(KEY_POS0)?,
            setup_hold: decode_setup_hold(flags)?,
            start_end: decode_start_end(flags >> 2)?,
            edge: decode_edge(flags >> 4)?,
            from: find_end(KEY_FROM)?,
            through: find_ends(KEY_THROUGH)?,
            to: find_end(KEY_TO)?,
            comment: find_str(KEY_COMMENT)?,
        }),
        OP_SET_MAX_DELAY => Ok(CmdKind::SetMaxDelay {
            delay: req_num(KEY_POS0)?,
            edge: decode_edge(flags)?,
            from: find_end(KEY_FROM)?,
            through: find_ends(KEY_THROUGH)?,
            to: find_end(KEY_TO)?,
            ignore_clock_latency: flags & (1 << 2) != 0,
            comment: find_str(KEY_COMMENT)?,
        }),
        OP_SET_MIN_DELAY => Ok(CmdKind::SetMinDelay {
            delay: req_num(KEY_POS0)?,
            edge: decode_edge(flags)?,
            from: find_end(KEY_FROM)?,
            through: find_ends(KEY_THROUGH)?,
            to: find_end(KEY_TO)?,
            ignore_clock_latency: flags & (1 << 2) != 0,
            comment: find_str(KEY_COMMENT)?,
        }),
        OP_SET_CASE_ANALYSIS => {
            let value = match find_enum(KEY_POS0) {
                Some(0) => CaseValue::Zero,
                Some(1) => CaseValue::One,
                Some(2) => CaseValue::Rise,
                Some(3) => CaseValue::Fall,
                other => {
                    return Err(ConstrError(format!(
                        "invalid case_analysis value {other:?}"
                    )));
                }
            };
            Ok(CmdKind::SetCaseAnalysis {
                value,
                ports_or_pins: req_obj(KEY_OBJECTS)?,
            })
        }
        OP_SET_LOGIC_ZERO => Ok(CmdKind::SetLogicZero {
            ports: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_LOGIC_ONE => Ok(CmdKind::SetLogicOne {
            ports: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_DISABLE_TIMING => Ok(CmdKind::SetDisableTiming {
            from: find_str(KEY_FROM)?,
            to: find_str(KEY_TO)?,
            objects: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_MAX_TIME_BORROW => Ok(CmdKind::SetMaxTimeBorrow {
            value: req_num(KEY_POS0)?,
            objects: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_TIMING_DERATE => Ok(CmdKind::SetTimingDerate {
            factor: req_num(KEY_POS0)?,
            early_late: decode_early_late(flags)?,
            edge: decode_edge(flags >> 2)?,
            clock: flags & (1 << 4) != 0,
            data: flags & (1 << 5) != 0,
            net_delay: flags & (1 << 6) != 0,
            cell_delay: flags & (1 << 7) != 0,
            cell_check: flags & (1 << 8) != 0,
            static_derate: flags & (1 << 9) != 0,
            dynamic: flags & (1 << 10) != 0,
            increment: flags & (1 << 11) != 0,
            objects: find_obj(KEY_OBJECTS)?,
        }),
        OP_SET_MIN_PULSE_WIDTH => Ok(CmdKind::SetMinPulseWidth {
            value: req_num(KEY_POS0)?,
            high: flags & (1 << 0) != 0,
            low: flags & (1 << 1) != 0,
            objects: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_IDEAL_NETWORK => Ok(CmdKind::SetIdealNetwork {
            no_propagate: flags & (1 << 0) != 0,
            objects: req_obj(KEY_OBJECTS)?,
        }),
        OP_GROUP_PATH => Ok(CmdKind::GroupPath {
            name: find_str(KEY_NAME)?,
            default: flags & (1 << 0) != 0,
            weight: find_num(KEY_WEIGHT),
            critical_range: find_num(KEY_CRITICAL_RANGE),
            from: find_end(KEY_FROM)?,
            through: find_ends(KEY_THROUGH)?,
            to: find_end(KEY_TO)?,
            comment: find_str(KEY_COMMENT)?,
        }),
        OP_SET_MAX_TRANSITION => Ok(CmdKind::SetMaxTransition {
            value: req_num(KEY_POS0)?,
            clock_path: flags & (1 << 0) != 0,
            data_path: flags & (1 << 1) != 0,
            edge: decode_edge(flags >> 2)?,
            objects: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_MAX_CAPACITANCE => Ok(CmdKind::SetMaxCapacitance {
            value: req_num(KEY_POS0)?,
            clock_path: flags & (1 << 0) != 0,
            data_path: flags & (1 << 1) != 0,
            edge: decode_edge(flags >> 2)?,
            objects: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_MIN_CAPACITANCE => Ok(CmdKind::SetMinCapacitance {
            value: req_num(KEY_POS0)?,
            clock_path: flags & (1 << 0) != 0,
            data_path: flags & (1 << 1) != 0,
            edge: decode_edge(flags >> 2)?,
            objects: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_MAX_FANOUT => Ok(CmdKind::SetMaxFanout {
            value: req_num(KEY_POS0)?,
            objects: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_FANOUT_LOAD => Ok(CmdKind::SetFanoutLoad {
            value: req_num(KEY_POS0)?,
            ports: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_PORT_FANOUT_NUMBER => Ok(CmdKind::SetPortFanoutNumber {
            value: req_int(KEY_POS0)?,
            ports: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_DONT_USE => Ok(CmdKind::SetDontUse {
            lib_cells: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_DONT_TOUCH => Ok(CmdKind::SetDontTouch {
            objects: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_DONT_TOUCH_NETWORK => Ok(CmdKind::SetDontTouchNetwork {
            no_propagate: flags & (1 << 0) != 0,
            objects: req_obj(KEY_OBJECTS)?,
        }),
        OP_SET_SIZE_ONLY => Ok(CmdKind::SetSizeOnly {
            instances: req_obj(KEY_OBJECTS)?,
            no_instances: flags & (1 << 0) != 0,
        }),
        OP_SET_MAX_AREA => Ok(CmdKind::SetMaxArea {
            value: req_num(KEY_POS0)?,
            ignore_tns: flags & (1 << 0) != 0,
        }),
        OP_SET_MAX_DYNAMIC_POWER => Ok(CmdKind::SetMaxDynamicPower {
            value: req_num(KEY_POS0)?,
            unit: find_str(KEY_POS1)?,
        }),
        OP_SET_MAX_LEAKAGE_POWER => Ok(CmdKind::SetMaxLeakagePower {
            value: req_num(KEY_POS0)?,
            unit: find_str(KEY_POS1)?,
        }),
        other => Err(ConstrError(format!("unknown CMDS opcode {other}"))),
    }
}
