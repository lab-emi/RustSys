// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Container-agnostic `CONSTR v1` binary section encoder and decoder (`Sdc::to_sections` and
//! `Sdc::from_sections`; SDC-REQ-0040 to SDC-REQ-0050).

use std::collections::HashMap;

use super::cmd::ExceptionEnd;
use super::obj::Obj;
use encode::encode_edge;

/// Section IDs of `CONSTR v1` (SDC-REQ-0041).
pub mod section_id {
    pub const STR_BYTES: u16 = 1;
    pub const STR_OFFS: u16 = 2;
    pub const HEADER: u16 = 3;
    pub const SOURCES: u16 = 4;
    pub const ENV: u16 = 5;
    pub const CMDS: u16 = 6;
    pub const ARGS: u16 = 7;
    pub const F64: u16 = 8;
    pub const OBJS: u16 = 9;
    pub const OBJ_CHILD: u16 = 10;
    pub const PATS: u16 = 11;
    pub const CANON: u16 = 12;
    pub const DIAGS: u16 = 13;
}

/// One typed binary section of a `CONSTR v1` table (SDC-REQ-0040).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstrSection {
    pub id: u16,
    pub elem_size: u32,
    pub count: u64,
    pub bytes: Vec<u8>,
}

/// Error returned when decoding or validating `CONSTR v1` sections (SDC-REQ-0049).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("corrupt CONSTR v1 table: {0}")]
pub struct ConstrError(pub String);

const NONE_U32: u32 = u32::MAX;

// Command opcodes in CMDS section
const OP_SET_UNITS: u16 = 1;
const OP_CURRENT_DESIGN: u16 = 2;
const OP_SET_OP_COND: u16 = 3;
const OP_SET_WLM: u16 = 4;
const OP_SET_WL_MODE: u16 = 5;
const OP_SET_WLSG: u16 = 6;
const OP_CREATE_CLOCK: u16 = 7;
const OP_CREATE_GEN_CLOCK: u16 = 8;
const OP_SET_PROP_CLOCK: u16 = 9;
const OP_UNSET_PROP_CLOCK: u16 = 10;
const OP_SET_CLOCK_GROUPS: u16 = 11;
const OP_SET_CLOCK_UNCERTAINTY: u16 = 12;
const OP_SET_CLOCK_LATENCY: u16 = 13;
const OP_SET_CLOCK_TRANSITION: u16 = 14;
const OP_SET_INPUT_DELAY: u16 = 15;
const OP_SET_OUTPUT_DELAY: u16 = 16;
const OP_SET_INPUT_TRANSITION: u16 = 17;
const OP_SET_DRIVING_CELL: u16 = 18;
const OP_SET_LOAD: u16 = 19;
const OP_SET_FALSE_PATH: u16 = 20;
const OP_SET_MULTICYCLE_PATH: u16 = 21;
const OP_SET_MAX_DELAY: u16 = 22;
const OP_SET_MIN_DELAY: u16 = 23;
const OP_SET_CASE_ANALYSIS: u16 = 24;
const OP_SET_LOGIC_ZERO: u16 = 25;
const OP_SET_LOGIC_ONE: u16 = 26;
const OP_SET_DISABLE_TIMING: u16 = 27;
const OP_SET_MAX_TIME_BORROW: u16 = 28;
const OP_SET_TIMING_DERATE: u16 = 29;
const OP_SET_MIN_PULSE_WIDTH: u16 = 30;
const OP_SET_IDEAL_NETWORK: u16 = 31;
const OP_GROUP_PATH: u16 = 32;
const OP_SET_MAX_TRANSITION: u16 = 33;
const OP_SET_MAX_CAPACITANCE: u16 = 34;
const OP_SET_MIN_CAPACITANCE: u16 = 35;
const OP_SET_MAX_FANOUT: u16 = 36;
const OP_SET_FANOUT_LOAD: u16 = 37;
const OP_SET_PORT_FANOUT_NUMBER: u16 = 38;
const OP_SET_DONT_USE: u16 = 39;
const OP_SET_DONT_TOUCH: u16 = 40;
const OP_SET_DONT_TOUCH_NETWORK: u16 = 41;
const OP_SET_SIZE_ONLY: u16 = 42;
const OP_SET_MAX_AREA: u16 = 43;
const OP_SET_MAX_DYNAMIC_POWER: u16 = 44;
const OP_SET_MAX_LEAKAGE_POWER: u16 = 45;

// Arg kinds in ARGS section
const ARG_NUM: u8 = 1;
const ARG_STR: u8 = 2;
const ARG_OBJ: u8 = 3;
const ARG_INT: u8 = 4;
const ARG_F64_LIST: u8 = 5;
const ARG_INT_TRIPLE: u8 = 6;
const ARG_ENUM: u8 = 7;

// Arg option keys
const KEY_POS0: u16 = 1;
const KEY_POS1: u16 = 2;
const KEY_NAME: u16 = 3;
const KEY_PERIOD: u16 = 4;
const KEY_WAVEFORM: u16 = 5;
const KEY_COMMENT: u16 = 6;
const KEY_LIBRARY: u16 = 7;
const KEY_MIN: u16 = 8;
const KEY_MAX: u16 = 9;
const KEY_MIN_LIB: u16 = 10;
const KEY_MAX_LIB: u16 = 11;
const KEY_ANALYSIS_TYPE: u16 = 12;
const KEY_OBJECTS: u16 = 13;
const KEY_SOURCE: u16 = 14;
const KEY_MASTER_CLOCK: u16 = 15;
const KEY_DIVIDE_BY: u16 = 16;
const KEY_MULTIPLY_BY: u16 = 17;
const KEY_DUTY_CYCLE: u16 = 18;
const KEY_EDGES: u16 = 19;
const KEY_EDGE_SHIFT: u16 = 20;
const KEY_GROUP: u16 = 21;
const KEY_FROM: u16 = 22;
const KEY_THROUGH: u16 = 23;
const KEY_TO: u16 = 24;
const KEY_CLOCK: u16 = 25;
const KEY_REFERENCE_PIN: u16 = 26;
const KEY_LIB_CELL: u16 = 27;
const KEY_PIN: u16 = 28;
const KEY_FROM_PIN: u16 = 29;
const KEY_IN_TRANS_RISE: u16 = 30;
const KEY_IN_TRANS_FALL: u16 = 31;
const KEY_WEIGHT: u16 = 32;
const KEY_CRITICAL_RANGE: u16 = 33;
const KEY_UNIT_TIME: u16 = 34;
const KEY_UNIT_CAP: u16 = 35;
const KEY_UNIT_RES: u16 = 36;
const KEY_UNIT_VOLT: u16 = 37;
const KEY_UNIT_CURR: u16 = 38;
const KEY_UNIT_POWER: u16 = 39;

// Obj kinds in OBJS section
const OBJ_PORTS: u8 = 1;
const OBJ_PINS: u8 = 2;
const OBJ_CELLS: u8 = 3;
const OBJ_NETS: u8 = 4;
const OBJ_CLOCKS: u8 = 5;
const OBJ_LIB_CELLS: u8 = 6;
const OBJ_LIB_PINS: u8 = 7;
const OBJ_ALL_INPUTS: u8 = 8;
const OBJ_ALL_OUTPUTS: u8 = 9;
const OBJ_ALL_CLOCKS: u8 = 10;
const OBJ_ALL_REGISTERS: u8 = 11;
const OBJ_CURRENT_DESIGN: u8 = 12;
const OBJ_NAMES: u8 = 13;
const OBJ_UNION: u8 = 14;
const OBJ_DIFF: u8 = 15;

const OBJ_FLAG_HIER: u8 = 0x01;
const OBJ_FLAG_REGEXP: u8 = 0x02;
const OBJ_FLAG_NOCASE: u8 = 0x04;
const OBJ_FLAG_NO_CLOCKS: u8 = 0x08;
const OBJ_FLAG_QUIET: u8 = 0x10;
const OBJ_FLAG_OF_OBJECTS: u8 = 0x20;

#[derive(Default)]
struct StrPool {
    bytes: Vec<u8>,
    offs: Vec<u32>,
    map: HashMap<String, u32>,
}

impl StrPool {
    fn new() -> Self {
        let mut p = Self {
            bytes: Vec::new(),
            offs: vec![0, 0],
            map: HashMap::new(),
        };
        p.map.insert(String::new(), 0);
        p
    }

    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = (self.offs.len() - 1) as u32;
        self.bytes.extend_from_slice(s.as_bytes());
        self.offs.push(self.bytes.len() as u32);
        self.map.insert(s.to_string(), id);
        id
    }

    fn intern_opt(&mut self, s: &Option<String>) -> u32 {
        match s {
            Some(v) => self.intern(v),
            None => NONE_U32,
        }
    }
}

#[derive(Default)]
struct Encoder {
    strs: StrPool,
    sources_bytes: Vec<u8>,
    sources_count: u64,
    env_bytes: Vec<u8>,
    env_count: u64,
    cmds_bytes: Vec<u8>,
    cmds_count: u64,
    args_bytes: Vec<u8>,
    args_count: u64,
    f64_bytes: Vec<u8>,
    f64_count: u64,
    objs_bytes: Vec<u8>,
    objs_count: u64,
    obj_child_bytes: Vec<u8>,
    obj_child_count: u64,
    pats_bytes: Vec<u8>,
    pats_count: u64,
    diags_bytes: Vec<u8>,
    diags_count: u64,
}

impl Encoder {
    fn new() -> Self {
        Self {
            strs: StrPool::new(),
            ..Self::default()
        }
    }

    fn push_arg(&mut self, key: u16, kind: u8, a: u32, b: u64) {
        self.args_bytes.extend_from_slice(&key.to_le_bytes());
        self.args_bytes.push(kind);
        self.args_bytes.push(0);
        self.args_bytes.extend_from_slice(&a.to_le_bytes());
        self.args_bytes.extend_from_slice(&b.to_le_bytes());
        self.args_count += 1;
    }

    fn push_arg_num(&mut self, key: u16, v: f64) {
        self.push_arg(key, ARG_NUM, 0, v.to_bits());
    }

    fn push_arg_str(&mut self, key: u16, s: &str) {
        let id = self.strs.intern(s);
        self.push_arg(key, ARG_STR, 0, id as u64);
    }

    fn push_arg_opt_str(&mut self, key: u16, s: &Option<String>) {
        if let Some(v) = s {
            self.push_arg_str(key, v);
        }
    }

    fn push_arg_int(&mut self, key: u16, v: u32) {
        self.push_arg(key, ARG_INT, 0, v as u64);
    }

    fn push_arg_enum(&mut self, key: u16, v: u32) {
        self.push_arg(key, ARG_ENUM, 0, v as u64);
    }

    fn push_arg_obj(&mut self, key: u16, obj: &Obj) {
        let id = self.encode_obj(obj);
        self.push_arg(key, ARG_OBJ, 0, id as u64);
    }

    fn push_arg_opt_obj(&mut self, key: u16, obj: &Option<Obj>) {
        if let Some(o) = obj {
            self.push_arg_obj(key, o);
        }
    }

    fn push_arg_end(&mut self, key: u16, end: &Option<ExceptionEnd>) {
        if let Some(e) = end {
            let id = self.encode_obj(&e.obj);
            let edge_code = encode_edge(e.edge);
            self.push_arg(key, ARG_OBJ, edge_code, id as u64);
        }
    }

    fn push_arg_f64_slice(&mut self, key: u16, vals: &[f64]) {
        let start = self.f64_count as u32;
        for &v in vals {
            self.f64_bytes.extend_from_slice(&v.to_bits().to_le_bytes());
            self.f64_count += 1;
        }
        self.push_arg(key, ARG_F64_LIST, start, vals.len() as u64);
    }

    fn push_pat(&mut self, s: &str) {
        let id = self.strs.intern(s);
        self.pats_bytes.extend_from_slice(&id.to_le_bytes());
        self.pats_count += 1;
    }

    fn push_obj_child(&mut self, id: u32) {
        self.obj_child_bytes.extend_from_slice(&id.to_le_bytes());
        self.obj_child_count += 1;
    }

    fn write_obj_row(&mut self, kind: u8, flags: u8, pad: u16, a: u32, b: u32, filter: u32) -> u32 {
        let id = self.objs_count as u32;
        self.objs_bytes.push(kind);
        self.objs_bytes.push(flags);
        self.objs_bytes.extend_from_slice(&pad.to_le_bytes());
        self.objs_bytes.extend_from_slice(&a.to_le_bytes());
        self.objs_bytes.extend_from_slice(&b.to_le_bytes());
        self.objs_bytes.extend_from_slice(&filter.to_le_bytes());
        self.objs_count += 1;
        id
    }
}

#[inline]
pub(super) const fn read_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

#[inline]
pub(super) const fn read_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

#[inline]
pub(super) const fn read_u64(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        b[off],
        b[off + 1],
        b[off + 2],
        b[off + 3],
        b[off + 4],
        b[off + 5],
        b[off + 6],
        b[off + 7],
    ])
}

// Pure-Rust scalar XXH3-64 implementation matching `xxhash_rust::xxh3::xxh3_64` (WS-REQ-0010).
const PRIME32_1: u32 = 0x9E37_79B1;
const PRIME32_2: u32 = 0x85EB_CA77;
const PRIME32_3: u32 = 0xC2B2_AE3D;
const PRIME64_1: u64 = 0x9E37_79B1_85EB_CA87;
const PRIME64_2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const PRIME64_3: u64 = 0x1656_67B1_9E37_79F9;
const PRIME64_4: u64 = 0x85EB_CA77_C2B2_AE63;
const PRIME64_5: u64 = 0x27D4_EB2F_1656_67C5;

const DEFAULT_SECRET: [u8; 192] = [
    0xb8, 0xfe, 0x6c, 0x39, 0x23, 0xa4, 0x4b, 0xbe, 0x7c, 0x01, 0x81, 0x2c, 0xf7, 0x21, 0xad, 0x1c,
    0xde, 0xd4, 0x6d, 0xe9, 0x83, 0x90, 0x97, 0xdb, 0x72, 0x40, 0xa4, 0xa4, 0xb7, 0xb3, 0x67, 0x1f,
    0xcb, 0x79, 0xe6, 0x4e, 0xcc, 0xc0, 0xe5, 0x78, 0x82, 0x5a, 0xd0, 0x7d, 0xcc, 0xff, 0x72, 0x21,
    0xb8, 0x08, 0x46, 0x74, 0xf7, 0x43, 0x24, 0x8e, 0xe0, 0x35, 0x90, 0xe6, 0x81, 0x3a, 0x26, 0x4c,
    0x3c, 0x28, 0x52, 0xbb, 0x91, 0xc3, 0x00, 0xcb, 0x88, 0xd0, 0x65, 0x8b, 0x1b, 0x53, 0x2e, 0xa3,
    0x71, 0x64, 0x48, 0x97, 0xa2, 0x0d, 0xf9, 0x4e, 0x38, 0x19, 0xef, 0x46, 0xa9, 0xde, 0xac, 0xd8,
    0xa8, 0xfa, 0x76, 0x3f, 0xe3, 0x9c, 0x34, 0x3f, 0xf9, 0xdc, 0xbb, 0xc7, 0xc7, 0x0b, 0x4f, 0x1d,
    0x8a, 0x51, 0xe0, 0x4b, 0xcd, 0xb4, 0x59, 0x31, 0xc8, 0x9f, 0x7e, 0xc9, 0xd9, 0x78, 0x73, 0x64,
    0xea, 0xc5, 0xac, 0x83, 0x34, 0xd3, 0xeb, 0xc3, 0xc5, 0x81, 0xa0, 0xff, 0xfa, 0x13, 0x63, 0xeb,
    0x17, 0x0d, 0xdd, 0x51, 0xb7, 0xf0, 0xda, 0x49, 0xd3, 0x16, 0x55, 0x26, 0x29, 0xd4, 0x68, 0x9e,
    0x2b, 0x16, 0xbe, 0x58, 0x7d, 0x47, 0xa1, 0xfc, 0x8f, 0xf8, 0xb8, 0xd1, 0x7a, 0xd0, 0x31, 0xce,
    0x45, 0xcb, 0x3a, 0x8f, 0x95, 0x16, 0x04, 0x28, 0xaf, 0xd7, 0xfb, 0xca, 0xbb, 0x4b, 0x40, 0x7e,
];

const INITIAL_ACC: [u64; 8] = [
    PRIME32_3 as u64,
    PRIME64_1,
    PRIME64_2,
    PRIME64_3,
    PRIME64_4,
    PRIME32_2 as u64,
    PRIME64_5,
    PRIME32_1 as u64,
];

const fn xorshift64(v: u64, shift: u64) -> u64 {
    v ^ (v >> shift)
}

const fn xxh64_avalanche(mut v: u64) -> u64 {
    v ^= v >> 33;
    v = v.wrapping_mul(PRIME64_2);
    v ^= v >> 29;
    v = v.wrapping_mul(PRIME64_3);
    v ^= v >> 32;
    v
}

const fn xxh3_avalanche(mut v: u64) -> u64 {
    v = xorshift64(v, 37);
    v = v.wrapping_mul(0x1656_6791_9E37_79F9);
    xorshift64(v, 32)
}

const fn strong_avalanche(mut v: u64, len: u64) -> u64 {
    v ^= v.rotate_left(49) ^ v.rotate_left(24);
    v = v.wrapping_mul(0x9FB2_1C65_1E98_DF25);
    v ^= (v >> 35).wrapping_add(len);
    v = v.wrapping_mul(0x9FB2_1C65_1E98_DF25);
    xorshift64(v, 28)
}

const fn mul128_fold64(lhs: u64, rhs: u64) -> u64 {
    let prod = (lhs as u128) * (rhs as u128);
    (prod as u64) ^ ((prod >> 64) as u64)
}

const fn mix16_b(input: &[u8], in_off: usize, secret_off: usize) -> u64 {
    let lo = read_u64(input, in_off) ^ read_u64(&DEFAULT_SECRET, secret_off);
    let hi = read_u64(input, in_off + 8) ^ read_u64(&DEFAULT_SECRET, secret_off + 8);
    mul128_fold64(lo, hi)
}

/// Computes the 64-bit `xxh3` hash of `input` (matching `rusteda_workspace::Hash64::of`).
pub const fn xxh3_64(input: &[u8]) -> u64 {
    let len = input.len();
    if len == 0 {
        xxh64_avalanche(read_u64(&DEFAULT_SECRET, 56) ^ read_u64(&DEFAULT_SECRET, 64))
    } else if len <= 3 {
        let combo = ((input[0] as u32) << 16)
            | ((input[len >> 1] as u32) << 24)
            | (input[len - 1] as u32)
            | ((len as u32) << 8);
        let flip = (read_u32(&DEFAULT_SECRET, 0) ^ read_u32(&DEFAULT_SECRET, 4)) as u64;
        xxh64_avalanche((combo as u64) ^ flip)
    } else if len <= 8 {
        let in1 = read_u32(input, 0);
        let in2 = read_u32(input, len - 4);
        let flip = read_u64(&DEFAULT_SECRET, 8) ^ read_u64(&DEFAULT_SECRET, 16);
        let in64 = (in2 as u64).wrapping_add((in1 as u64) << 32);
        strong_avalanche(in64 ^ flip, len as u64)
    } else if len <= 16 {
        let flip1 = read_u64(&DEFAULT_SECRET, 24) ^ read_u64(&DEFAULT_SECRET, 32);
        let flip2 = read_u64(&DEFAULT_SECRET, 40) ^ read_u64(&DEFAULT_SECRET, 48);
        let lo = read_u64(input, 0) ^ flip1;
        let hi = read_u64(input, len - 8) ^ flip2;
        let acc = (len as u64)
            .wrapping_add(lo.swap_bytes())
            .wrapping_add(hi)
            .wrapping_add(mul128_fold64(lo, hi));
        xxh3_avalanche(acc)
    } else if len <= 128 {
        let mut acc = (len as u64).wrapping_mul(PRIME64_1);
        if len > 32 {
            if len > 64 {
                if len > 96 {
                    acc = acc.wrapping_add(mix16_b(input, 48, 96));
                    acc = acc.wrapping_add(mix16_b(input, len - 64, 112));
                }
                acc = acc.wrapping_add(mix16_b(input, 32, 64));
                acc = acc.wrapping_add(mix16_b(input, len - 48, 80));
            }
            acc = acc.wrapping_add(mix16_b(input, 16, 32));
            acc = acc.wrapping_add(mix16_b(input, len - 32, 48));
        }
        acc = acc.wrapping_add(mix16_b(input, 0, 0));
        acc = acc.wrapping_add(mix16_b(input, len - 16, 16));
        xxh3_avalanche(acc)
    } else if len <= 240 {
        let mut acc = (len as u64).wrapping_mul(PRIME64_1);
        let nb_rounds = len / 16;
        let mut i = 0;
        while i < 8 {
            acc = acc.wrapping_add(mix16_b(input, 16 * i, 16 * i));
            i += 1;
        }
        acc = xxh3_avalanche(acc);
        while i < nb_rounds {
            acc = acc.wrapping_add(mix16_b(input, 16 * i, 16 * (i - 8) + 3));
            i += 1;
        }
        acc = acc.wrapping_add(mix16_b(input, len - 16, 136 - 17));
        xxh3_avalanche(acc)
    } else {
        let mut acc = INITIAL_ACC;
        let nb_stripes = (192 - 64) / 8; // 16
        let block_len = 64 * nb_stripes; // 1024
        let nb_blocks = (len - 1) / block_len;
        let mut b_idx = 0;
        while b_idx < nb_blocks {
            let mut s_idx = 0;
            while s_idx < nb_stripes {
                acc = accumulate_512(acc, input, b_idx * block_len + s_idx * 64, s_idx * 8);
                s_idx += 1;
            }
            acc = scramble_acc(acc, 192 - 64);
            b_idx += 1;
        }
        let rem_stripes = ((len - 1) - (block_len * nb_blocks)) / 64;
        let mut s_idx = 0;
        while s_idx < rem_stripes {
            acc = accumulate_512(acc, input, nb_blocks * block_len + s_idx * 64, s_idx * 8);
            s_idx += 1;
        }
        acc = accumulate_512(acc, input, len - 64, 192 - 64 - 7);
        let mut result = (len as u64).wrapping_mul(PRIME64_1);
        let mut i = 0;
        while i < 4 {
            let a0 = acc[i * 2] ^ read_u64(&DEFAULT_SECRET, 11 + i * 16);
            let a1 = acc[i * 2 + 1] ^ read_u64(&DEFAULT_SECRET, 11 + i * 16 + 8);
            result = result.wrapping_add(mul128_fold64(a0, a1));
            i += 1;
        }
        xxh3_avalanche(result)
    }
}

const fn accumulate_512(
    mut acc: [u64; 8],
    input: &[u8],
    in_off: usize,
    secret_off: usize,
) -> [u64; 8] {
    let mut i = 0;
    while i < 8 {
        let data_val = read_u64(input, in_off + 8 * i);
        let data_key = data_val ^ read_u64(&DEFAULT_SECRET, secret_off + 8 * i);
        acc[i ^ 1] = acc[i ^ 1].wrapping_add(data_val);
        let prod = (data_key & 0xFFFF_FFFF).wrapping_mul(data_key >> 32);
        acc[i] = acc[i].wrapping_add(prod);
        i += 1;
    }
    acc
}

const fn scramble_acc(mut acc: [u64; 8], secret_off: usize) -> [u64; 8] {
    let mut i = 0;
    while i < 8 {
        let key = read_u64(&DEFAULT_SECRET, secret_off + 8 * i);
        let v = (xorshift64(acc[i], 47) ^ key).wrapping_mul(PRIME32_1 as u64);
        acc[i] = v;
        i += 1;
    }
    acc
}

mod decode;
mod encode;

pub use decode::decode_sections;
pub use encode::encode_sections;
