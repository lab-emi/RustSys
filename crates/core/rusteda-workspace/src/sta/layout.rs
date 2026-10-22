// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Section ids, element sizes and the fixed-size records of the `STA` v1 table
//! (WS-REQ-0126, WS-REQ-0127).
//!
//! Every record is `#[repr(C)]` plain old data without padding, so a section is viewed in place
//! as `&[Record]` from a map or an aligned buffer. The field order is the byte order of the
//! specification (`docs/specs/workspace/sta.md`); the compile-time asserts pin the sizes.

use bytemuck::{Pod, Zeroable};

/// Section ids of `STA` v1, `1..=24` (WS-REQ-0126).
pub(crate) mod section {
    pub(crate) const STR_BYTES: u32 = 1;
    pub(crate) const STR_OFFS: u32 = 2;
    pub(crate) const META: u32 = 3;
    pub(crate) const CORNERS: u32 = 4;
    pub(crate) const CLOCKS: u32 = 5;
    pub(crate) const TAGS: u32 = 6;
    pub(crate) const DISABLED_ARCS: u32 = 7;
    pub(crate) const NODE_REC: u32 = 8;
    pub(crate) const TAG_EXTRA_PTR: u32 = 9;
    pub(crate) const TAG_EXTRA: u32 = 10;
    pub(crate) const ENDPOINTS: u32 = 11;
    pub(crate) const EP_SLACK: u32 = 12;
    pub(crate) const EP_ORDER: u32 = 13;
    pub(crate) const PATH_RANK: u32 = 14;
    pub(crate) const PATH_CHAIN_PTR: u32 = 15;
    pub(crate) const PATH_CHAIN: u32 = 16;
    pub(crate) const PATHS: u32 = 17;
    pub(crate) const PATH_POINTS: u32 = 18;
    pub(crate) const NAME_HASH: u32 = 19;
    pub(crate) const INST_HASH: u32 = 20;
    pub(crate) const SUMMARY: u32 = 21;
    pub(crate) const UNCONSTRAINED: u32 = 22;
    pub(crate) const NOTES: u32 = 23;
    pub(crate) const SCOPE: u32 = 24;

    /// The number of sections.
    pub(crate) const COUNT: u32 = 24;

    /// The element size of section `id`, `None` outside `1..=24`.
    pub(crate) const fn elem_size(id: u32) -> Option<u32> {
        Some(match id {
            STR_BYTES | SCOPE => 1,
            STR_OFFS | TAG_EXTRA_PTR | EP_ORDER | PATH_CHAIN_PTR | PATH_CHAIN | NAME_HASH
            | INST_HASH => 4,
            PATH_RANK => 8,
            UNCONSTRAINED => 12,
            CORNERS | TAGS | DISABLED_ARCS | EP_SLACK | NOTES => 16,
            ENDPOINTS | PATH_POINTS => 32,
            CLOCKS => 48,
            TAG_EXTRA | PATHS => 56,
            META => 64,
            SUMMARY => 80,
            NODE_REC => 152,
            _ => return None,
        })
    }

    /// The name of section `id`, for messages.
    pub(crate) const fn name(id: u32) -> &'static str {
        match id {
            STR_BYTES => "STR_BYTES",
            STR_OFFS => "STR_OFFS",
            META => "META",
            CORNERS => "CORNERS",
            CLOCKS => "CLOCKS",
            TAGS => "TAGS",
            DISABLED_ARCS => "DISABLED_ARCS",
            NODE_REC => "NODE_REC",
            TAG_EXTRA_PTR => "TAG_EXTRA_PTR",
            TAG_EXTRA => "TAG_EXTRA",
            ENDPOINTS => "ENDPOINTS",
            EP_SLACK => "EP_SLACK",
            EP_ORDER => "EP_ORDER",
            PATH_RANK => "PATH_RANK",
            PATH_CHAIN_PTR => "PATH_CHAIN_PTR",
            PATH_CHAIN => "PATH_CHAIN",
            PATHS => "PATHS",
            PATH_POINTS => "PATH_POINTS",
            NAME_HASH => "NAME_HASH",
            INST_HASH => "INST_HASH",
            SUMMARY => "SUMMARY",
            UNCONSTRAINED => "UNCONSTRAINED",
            NOTES => "NOTES",
            SCOPE => "SCOPE",
            _ => "unknown",
        }
    }
}

/// `META`: one record of counts and modes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub(crate) struct MetaRec {
    pub num_nodes: u32,
    pub num_port_bits: u32,
    pub num_arcs: u32,
    pub num_cells: u32,
    pub num_startpoints: u32,
    pub num_disabled_arcs: u32,
    pub num_corners: u32,
    pub num_clocks: u32,
    pub num_tags: u32,
    pub num_endpoints: u32,
    pub num_ranked_paths: u32,
    pub num_top_k_paths: u32,
    pub num_path_groups: u32,
    /// 0 full, 1 summary.
    pub store_mode: u32,
    pub backend_str: u32,
    /// 0 `f64`, 1 `f32` propagation.
    pub precision: u32,
}

/// `CORNERS`: one record per analysis corner.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub(crate) struct CornerRec {
    pub name_str: u32,
    pub lib_count: u32,
    pub flags: u32,
    pub reserved: u32,
}

/// `CLOCKS`: one record per clock.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub(crate) struct ClockRec {
    pub name_str: u32,
    pub source_node: u32,
    pub master_clock: u32,
    pub flags: u32,
    pub period_ns: f64,
    pub rise_edge_ns: f64,
    pub fall_edge_ns: f64,
    pub uncertainty_setup_ns: f32,
    pub uncertainty_hold_ns: f32,
}

/// `TAGS`: one record per launch tag.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub(crate) struct TagRec {
    pub clock_id: u32,
    pub launch_edge: u8,
    pub flags: u8,
    pub exc_state: u16,
    pub launch_source_node: u32,
    pub path_group_str: u32,
}

/// `NODE_REC`: timing values of one (corner, node), `[transition][early, late]` grids.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub(crate) struct NodeRec {
    pub slew: [[f64; 2]; 2],
    pub required: [[f64; 2]; 2],
    pub slack: [[f64; 2]; 2],
    pub tag: u32,
    /// The node's `NAME_HASH` bucket, or [`super::NO_INDEX`].
    pub name_bucket: u32,
    pub arrival: [[f64; 2]; 2],
    pub pred: [[u32; 2]; 2],
}

/// `ENDPOINTS`: one record per checked endpoint.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub(crate) struct EndpointRec {
    pub data_node: u32,
    pub clock_node: u32,
    pub capture_clock: u32,
    pub path_group_str: u32,
    pub check_arc: u32,
    /// Bits 0..8 kind, 8..10 capture edge, 10..32 `NAME_HASH` bucket + 1 (0: none).
    pub kind_flags: u32,
    pub worst_setup_rank: u32,
    pub worst_hold_rank: u32,
}

/// `EP_SLACK`: setup and hold slack of one (corner, endpoint); NaN when absent.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub(crate) struct EpSlackRec {
    pub setup_slack: f64,
    pub hold_slack: f64,
}

/// `PATH_RANK`: one record per ranked path, in rank order.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub(crate) struct PathRankRec {
    pub slack_ns: f32,
    /// Bits 0..16 of the endpoint index.
    pub endpoint_lo: u16,
    /// Bits 0..4 corner index, bits 4..8 bits 16..20 of the endpoint index.
    pub corner_ep_hi: u8,
    /// Bit 0 hold family, bit 1 end transition, bits 2..4 check kind, bit 7 materialized.
    pub flags: u8,
}

/// `PATHS`: the header of one materialized (top-K) path.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub(crate) struct PathHeaderRec {
    pub slack: f64,
    pub arrival: f64,
    pub required: f64,
    pub check_value: f64,
    pub rank: u32,
    pub endpoint: u32,
    pub launch_clock: u32,
    pub capture_clock: u32,
    pub point_begin: u32,
    pub point_end: u32,
}

/// `PATH_POINTS`: one point of a materialized path.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub(crate) struct PathPointRec {
    pub arrival: f64,
    pub incr_delay: f64,
    pub slew: f64,
    pub node: u32,
    pub transition: u8,
    pub kind: u8,
    pub pad: u16,
}

/// `SUMMARY`: one record per (corner, path group).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub(crate) struct SummaryRec {
    pub setup_wns: f64,
    pub setup_tns: f64,
    pub hold_wns: f64,
    pub hold_tns: f64,
    pub corner: u32,
    pub group_str: u32,
    pub setup_violating: u32,
    pub setup_endpoints: u32,
    pub hold_violating: u32,
    pub hold_endpoints: u32,
    pub max_trans_violating: u32,
    pub max_cap_violating: u32,
    pub total_endpoints: u32,
    pub unconstrained_endpoints: u32,
    pub reserved0: u32,
    pub reserved1: u32,
}

const _: () = {
    assert!(size_of::<MetaRec>() == 64);
    assert!(size_of::<CornerRec>() == 16);
    assert!(size_of::<ClockRec>() == 48);
    assert!(size_of::<TagRec>() == 16);
    assert!(size_of::<NodeRec>() == 152);
    assert!(size_of::<EndpointRec>() == 32);
    assert!(size_of::<EpSlackRec>() == 16);
    assert!(size_of::<PathRankRec>() == 8);
    assert!(size_of::<PathHeaderRec>() == 56);
    assert!(size_of::<PathPointRec>() == 32);
    assert!(size_of::<SummaryRec>() == 80);
};

/// Words of the header of every path chain: required time and check value, two `f64` as
/// little-endian word pairs.
pub(crate) const CHAIN_HEADER_WORDS: usize = 4;

/// Words per bucket of `NAME_HASH` and `INST_HASH`.
pub(crate) const BUCKET_WORDS: usize = 4;

/// The `f64` stored as the word pair `[lo, hi]`.
pub(crate) fn f64_from_words(lo: u32, hi: u32) -> f64 {
    f64::from_bits(u64::from(lo) | (u64::from(hi) << 32))
}

/// The word pair `[lo, hi]` of an `f64`.
pub(crate) fn f64_to_words(v: f64) -> [u32; 2] {
    let bits = v.to_bits();
    [bits as u32, (bits >> 32) as u32]
}

/// The probe start and the stored low word of a name's 64-bit hash in an open-addressed table
/// of `mask + 1` buckets: the slot is the high word masked, the stored word the low word, at
/// least 1 so that 0 marks an empty bucket (WS-REQ-0130).
pub(crate) fn hash_slot(hash: u64, mask: usize) -> (usize, u32) {
    (((hash >> 32) as usize) & mask, (hash as u32).max(1))
}

/// The bucket count of an open-addressed table holding `entries`: 0 when empty, else the next
/// power of two of at least twice the entries and 16 (WS-REQ-0135).
pub(crate) fn bucket_count(entries: usize) -> usize {
    if entries == 0 {
        0
    } else {
        entries.saturating_mul(2).max(16).next_power_of_two()
    }
}

/// `-0.0` written as `+0.0` and every NaN as the quiet NaN `0x7ff8_0000_0000_0000`, so that
/// equal tables have equal bytes (WS-REQ-0135).
pub(crate) fn canon(x: f64) -> f64 {
    if x == 0.0 {
        0.0
    } else if x.is_nan() {
        f64::from_bits(0x7ff8_0000_0000_0000)
    } else {
        x
    }
}
