// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The validating reader of `STA` v1 (WS-REQ-0126, WS-REQ-0128 to WS-REQ-0134).
//!
//! [`StaTable`] validates the structure of a table when it is opened, in time independent of
//! the table size (WS-REQ-0128), and every index that a query reads from the table before it
//! uses it (WS-REQ-0129). Queries borrow the sections in place; the only allocations are their
//! results and, when a name has to be found by node, one map of the occupied `NAME_HASH`
//! buckets, all bounded by the lengths of the sections.

use std::collections::HashMap;

use serde_json::{Map, Value};

use super::layout::{
    BUCKET_WORDS, CHAIN_HEADER_WORDS, ClockRec, CornerRec, EndpointRec, EpSlackRec, MetaRec,
    NodeRec, PathHeaderRec, PathPointRec, PathRankRec, SummaryRec, f64_from_words, hash_slot,
    section,
};
use super::{
    CheckKind, Clock, Counts, Endpoint, EndpointKind, KIND, NO_INDEX, PathPoint, PathRank,
    PointKind, RankedPath, StoreMode, SummaryRow, TABLE_NAME, Transition, VERSIONS,
};
use crate::error::{Corruption, Error, Result};
use crate::hash::Hash64;
use crate::rbin::Rbin;
use crate::workspace::Revision;

/// An opened, structurally validated timing table (WS-REQ-0126, WS-REQ-0128).
#[derive(Debug)]
pub struct StaTable {
    rbin: Rbin,
    meta: MetaRec,
    mode: StoreMode,
}

/// The sections that queries read, viewed in place.
struct View<'a> {
    str_bytes: &'a [u8],
    str_offs: &'a [u32],
    corners: &'a [CornerRec],
    clocks: &'a [ClockRec],
    node_rec: &'a [NodeRec],
    endpoints: &'a [EndpointRec],
    ep_slack: &'a [EpSlackRec],
    ep_order: &'a [u32],
    path_rank: &'a [PathRankRec],
    chain_ptr: &'a [u32],
    chain: &'a [u32],
    paths: &'a [PathHeaderRec],
    points: &'a [PathPointRec],
    name_hash: &'a [u32],
    summary: &'a [SummaryRec],
    scope: &'a [u8],
}

/// The name of a node as one `NAME_HASH` bucket holds it.
struct NodeName<'a> {
    node: u32,
    prefix: Option<&'a str>,
    leaf: &'a str,
}

impl NodeName<'_> {
    fn joined(&self) -> String {
        match self.prefix {
            Some(p) => format!("{p}/{}", self.leaf),
            None => self.leaf.to_string(),
        }
    }
}

/// `CORRUPT_TABLE` for the table at `rbin` with `reason`.
fn corrupt_at(rbin: &Rbin, reason: impl std::fmt::Display) -> Error {
    Error::corrupt(
        rbin.path(),
        Corruption::Json {
            reason: format!("STA v1 table: {reason}"),
        },
    )
}

/// The kind of a path point from its two-bit code (WS-REQ-0134).
fn point_kind(k: u32) -> PointKind {
    match k & 0b11 {
        0 => PointKind::Port,
        1 => PointKind::Net,
        2 => PointKind::Cell,
        _ => PointKind::ClockLaunch,
    }
}

/// The elements of section `id`, whose element size the directory check has fixed.
fn count(rbin: &Rbin, id: u32) -> Result<usize> {
    rbin.section_info(id)
        .map(|s| s.count() as usize)
        .ok_or_else(|| Error::corrupt(rbin.path(), Corruption::MissingSection { id }))
}

/// `a * b` for counts, `CORRUPT_TABLE` on overflow.
fn mul(rbin: &Rbin, a: usize, b: usize, what: &str) -> Result<usize> {
    a.checked_mul(b)
        .ok_or_else(|| corrupt_at(rbin, format!("{what} overflows")))
}

/// Validates a CSR row pointer of `rows + 1` entries into `target` elements: first 0, last
/// `target` (WS-REQ-0128). Inner entries are checked where a row is read (WS-REQ-0129).
fn check_csr(rbin: &Rbin, ptr: &[u32], rows: usize, target: usize, what: &str) -> Result<()> {
    let Some(want) = rows.checked_add(1) else {
        return Err(corrupt_at(rbin, format!("{what} row count overflows")));
    };
    if ptr.len() != want {
        return Err(corrupt_at(
            rbin,
            format!("{what} has {} entries, META gives {want}", ptr.len()),
        ));
    }
    if ptr.first() != Some(&0) || ptr.last().map(|&l| l as usize) != Some(target) {
        return Err(corrupt_at(
            rbin,
            format!("{what} must start at 0 and end at {target}"),
        ));
    }
    Ok(())
}

/// Validates an open-addressed table: whole buckets, zero or a power of two of them.
fn check_hash_table(rbin: &Rbin, id: u32) -> Result<()> {
    let words = count(rbin, id)?;
    let buckets = words / BUCKET_WORDS;
    if !words.is_multiple_of(BUCKET_WORDS) || (buckets != 0 && !buckets.is_power_of_two()) {
        return Err(corrupt_at(
            rbin,
            format!(
                "{} holds {words} words, not a power-of-two number of 4-word buckets",
                section::name(id)
            ),
        ));
    }
    Ok(())
}

/// The structural checks of an open (WS-REQ-0128).
fn validate(rbin: &Rbin) -> Result<(MetaRec, StoreMode)> {
    if rbin.kind() != KIND {
        let what = Corruption::WrongKind {
            expected: KIND.to_string(),
            found: rbin.kind().to_string(),
        };
        return Err(Error::corrupt(rbin.path(), what));
    }
    let version = rbin.format_version();
    if version > *VERSIONS.end() {
        return Err(Error::FormatTooNew {
            path: rbin.path().to_path_buf(),
            kind: KIND.to_string(),
            found: version,
            min: *VERSIONS.start(),
            max: *VERSIONS.end(),
        });
    }
    if !VERSIONS.contains(&version) {
        let what = Corruption::UnsupportedVersion {
            found: version,
            min: *VERSIONS.start(),
            max: *VERSIONS.end(),
        };
        return Err(Error::corrupt(rbin.path(), what));
    }
    let sections = rbin.sections();
    if sections.len() != section::COUNT as usize {
        return Err(corrupt_at(
            rbin,
            format!(
                "{} sections, expected the {} sections 1..=24",
                sections.len(),
                section::COUNT
            ),
        ));
    }
    for (i, s) in sections.iter().enumerate() {
        let id = i as u32 + 1;
        if s.id != id {
            return Err(corrupt_at(
                rbin,
                format!("section {} where section {id} belongs", s.id),
            ));
        }
        if Some(s.elem_size) != section::elem_size(id) {
            return Err(corrupt_at(
                rbin,
                format!(
                    "section {id} ({}) has {}-byte elements, expected {}",
                    section::name(id),
                    s.elem_size,
                    section::elem_size(id).unwrap_or(0)
                ),
            ));
        }
    }
    if let Some(last) = sections.last() {
        let end = last
            .offset
            .checked_add(last.len)
            .and_then(|e| e.checked_add(7))
            .map(|e| e & !7);
        if end != Some(rbin.file_len()) {
            return Err(corrupt_at(
                rbin,
                format!(
                    "the file has {} bytes, the sections end at {}",
                    rbin.file_len(),
                    end.unwrap_or(u64::MAX)
                ),
            ));
        }
    }
    let metas = rbin.section::<MetaRec>(section::META)?;
    let [m] = metas else {
        return Err(corrupt_at(
            rbin,
            format!("META has {} records, expected 1", metas.len()),
        ));
    };
    let mode = match m.store_mode {
        0 => StoreMode::Full,
        1 => StoreMode::Summary,
        x => return Err(corrupt_at(rbin, format!("store mode {x} is not 0 or 1"))),
    };
    if m.precision > 1 {
        return Err(corrupt_at(
            rbin,
            format!("precision {} is not 0 or 1", m.precision),
        ));
    }
    if m.num_port_bits > m.num_nodes {
        return Err(corrupt_at(
            rbin,
            format!("{} port bits but {} nodes", m.num_port_bits, m.num_nodes),
        ));
    }
    if m.num_top_k_paths > m.num_ranked_paths {
        return Err(corrupt_at(
            rbin,
            format!(
                "{} materialized paths but {} ranked paths",
                m.num_top_k_paths, m.num_ranked_paths
            ),
        ));
    }
    let offs = rbin.section::<u32>(section::STR_OFFS)?;
    let bytes = count(rbin, section::STR_BYTES)?;
    if offs.first() != Some(&0) || offs.last().map(|&l| l as usize) != Some(bytes) {
        return Err(corrupt_at(
            rbin,
            "STR_OFFS must start at 0 and end at the length of STR_BYTES",
        ));
    }
    let n = |v: u32| v as usize;
    let corners = n(m.num_corners);
    let endpoints = n(m.num_endpoints);
    let corner_endpoints = mul(rbin, corners, endpoints, "corners * endpoints")?;
    let equal = [
        (section::CORNERS, corners),
        (section::CLOCKS, n(m.num_clocks)),
        (section::TAGS, n(m.num_tags)),
        (section::DISABLED_ARCS, n(m.num_disabled_arcs)),
        (section::ENDPOINTS, endpoints),
        (section::EP_SLACK, corner_endpoints),
        (
            section::EP_ORDER,
            mul(rbin, corner_endpoints, 2, "EP_ORDER length")?,
        ),
        (section::PATH_RANK, n(m.num_ranked_paths)),
        (section::PATHS, n(m.num_top_k_paths)),
        (
            section::SUMMARY,
            mul(rbin, corners, n(m.num_path_groups), "corners * path groups")?,
        ),
    ];
    for (id, want) in equal {
        let have = count(rbin, id)?;
        if have != want {
            return Err(corrupt_at(
                rbin,
                format!(
                    "{} has {have} records, META gives {want}",
                    section::name(id)
                ),
            ));
        }
    }
    check_csr(
        rbin,
        rbin.section::<u32>(section::PATH_CHAIN_PTR)?,
        n(m.num_ranked_paths),
        count(rbin, section::PATH_CHAIN)?,
        "PATH_CHAIN_PTR",
    )?;
    let node_values = [
        section::NODE_REC,
        section::TAG_EXTRA_PTR,
        section::TAG_EXTRA,
    ];
    match mode {
        StoreMode::Full => {
            let rows = mul(rbin, corners, n(m.num_nodes), "corners * nodes")?;
            if count(rbin, section::NODE_REC)? != rows {
                return Err(corrupt_at(
                    rbin,
                    format!("NODE_REC must hold {rows} records in the full mode"),
                ));
            }
            check_csr(
                rbin,
                rbin.section::<u32>(section::TAG_EXTRA_PTR)?,
                rows,
                count(rbin, section::TAG_EXTRA)?,
                "TAG_EXTRA_PTR",
            )?;
        }
        StoreMode::Summary => {
            for id in node_values {
                if count(rbin, id)? != 0 {
                    return Err(corrupt_at(
                        rbin,
                        format!("{} must be empty in the summary mode", section::name(id)),
                    ));
                }
            }
        }
    }
    check_hash_table(rbin, section::NAME_HASH)?;
    check_hash_table(rbin, section::INST_HASH)?;
    if count(rbin, section::SCOPE)? < 2 {
        return Err(corrupt_at(rbin, "SCOPE holds less than a JSON object"));
    }
    Ok((*m, mode))
}

impl StaTable {
    /// Maps the timing table of `rev` through [`Revision::open_rbin`], which validates the rbin
    /// header and its upstream map (WS-REQ-0038), and validates its structure (WS-REQ-0126,
    /// WS-REQ-0128). A revision without the table is `MISSING_TABLE`.
    pub fn open_rev(rev: &Revision) -> Result<StaTable> {
        StaTable::from_rbin(rev.open_rbin(TABLE_NAME, KIND, VERSIONS)?)
    }

    /// Validates the structure of an opened rbin (WS-REQ-0128).
    pub fn from_rbin(rbin: Rbin) -> Result<StaTable> {
        let (meta, mode) = validate(&rbin)?;
        Ok(StaTable { rbin, meta, mode })
    }

    /// Opens table bytes held in memory (WS-REQ-0126, WS-REQ-0128).
    pub fn from_bytes(bytes: Vec<u8>) -> Result<StaTable> {
        StaTable::from_rbin(Rbin::from_bytes(bytes, KIND, VERSIONS)?)
    }

    /// Recomputes the content hash and compares it with the header (WS-REQ-0034); a tool that
    /// derives a change of the design from the table calls it first (WS-REQ-0128).
    pub fn verify_hash(&self) -> Result<()> {
        self.rbin.verify_hash()
    }

    /// The opened container.
    pub fn rbin(&self) -> &Rbin {
        &self.rbin
    }

    /// The store mode.
    pub fn store_mode(&self) -> StoreMode {
        self.mode
    }

    /// The counts of `META`.
    pub fn counts(&self) -> Counts {
        let m = &self.meta;
        Counts {
            nodes: m.num_nodes,
            port_bits: m.num_port_bits,
            cells: m.num_cells,
            corners: m.num_corners,
            clocks: m.num_clocks,
            endpoints: m.num_endpoints,
            ranked_paths: m.num_ranked_paths,
            top_k_paths: m.num_top_k_paths,
            path_groups: m.num_path_groups,
        }
    }

    fn corrupt(&self, reason: impl std::fmt::Display) -> Error {
        corrupt_at(&self.rbin, reason)
    }

    fn view(&self) -> Result<View<'_>> {
        let r = &self.rbin;
        Ok(View {
            str_bytes: r.section::<u8>(section::STR_BYTES)?,
            str_offs: r.section::<u32>(section::STR_OFFS)?,
            corners: r.section::<CornerRec>(section::CORNERS)?,
            clocks: r.section::<ClockRec>(section::CLOCKS)?,
            node_rec: r.section::<NodeRec>(section::NODE_REC)?,
            endpoints: r.section::<EndpointRec>(section::ENDPOINTS)?,
            ep_slack: r.section::<EpSlackRec>(section::EP_SLACK)?,
            ep_order: r.section::<u32>(section::EP_ORDER)?,
            path_rank: r.section::<PathRankRec>(section::PATH_RANK)?,
            chain_ptr: r.section::<u32>(section::PATH_CHAIN_PTR)?,
            chain: r.section::<u32>(section::PATH_CHAIN)?,
            paths: r.section::<PathHeaderRec>(section::PATHS)?,
            points: r.section::<PathPointRec>(section::PATH_POINTS)?,
            name_hash: r.section::<u32>(section::NAME_HASH)?,
            summary: r.section::<SummaryRec>(section::SUMMARY)?,
            scope: r.section::<u8>(section::SCOPE)?,
        })
    }

    /// String `id` of the string table (WS-REQ-0129).
    fn string<'a>(&self, v: &View<'a>, id: u32) -> Result<&'a str> {
        let i = id as usize;
        let bounds = i
            .checked_add(1)
            .and_then(|j| Some((*v.str_offs.get(i)?, *v.str_offs.get(j)?)));
        let Some((start, end)) = bounds else {
            return Err(self.corrupt(format!(
                "string {id} is out of range ({} strings)",
                v.str_offs.len().saturating_sub(1)
            )));
        };
        let bytes = v
            .str_bytes
            .get(start as usize..end as usize)
            .ok_or_else(|| self.corrupt(format!("string {id} has a bad byte range")))?;
        std::str::from_utf8(bytes).map_err(|_| self.corrupt(format!("string {id} is not UTF-8")))
    }

    /// The name that `NAME_HASH` bucket `b` holds (WS-REQ-0129, WS-REQ-0130).
    fn bucket<'a>(&self, v: &View<'a>, b: u32) -> Result<NodeName<'a>> {
        let words = (b as usize)
            .checked_mul(BUCKET_WORDS)
            .and_then(|at| v.name_hash.get(at..at.checked_add(BUCKET_WORDS)?));
        let Some(&[hash_lo, node, prefix, leaf]) = words else {
            return Err(self.corrupt(format!(
                "name bucket {b} is out of range ({} buckets)",
                v.name_hash.len() / BUCKET_WORDS
            )));
        };
        if hash_lo == 0 {
            return Err(self.corrupt(format!("name bucket {b} is empty")));
        }
        if node >= self.meta.num_nodes {
            return Err(self.corrupt(format!(
                "name bucket {b} names node {node} of {}",
                self.meta.num_nodes
            )));
        }
        let port = node < self.meta.num_port_bits;
        if port != (prefix == NO_INDEX) {
            return Err(self.corrupt(format!(
                "name bucket {b}: a port name has no instance prefix and a pin name has one \
                 (node {node})"
            )));
        }
        let prefix = if prefix == NO_INDEX {
            None
        } else {
            Some(self.string(v, prefix)?)
        };
        Ok(NodeName {
            node,
            prefix,
            leaf: self.string(v, leaf)?,
        })
    }

    /// The first occupied bucket of every node that `NAME_HASH` names; built once per query that
    /// needs a name by node, bounded by the buckets.
    fn buckets_by_node(&self, v: &View<'_>) -> HashMap<u32, u32> {
        let mut map = HashMap::new();
        for (b, words) in v.name_hash.chunks_exact(BUCKET_WORDS).enumerate() {
            if words[0] != 0 {
                map.entry(words[1]).or_insert(b as u32);
            }
        }
        map
    }

    /// The name of `node`: through `hint` (a bucket the table records for it), else through
    /// `NODE_REC` of the first corner, else through a scan of `NAME_HASH` (WS-REQ-0129).
    fn node_name<'a>(
        &self,
        v: &View<'a>,
        node: u32,
        hint: Option<u32>,
        scan: &mut Option<HashMap<u32, u32>>,
    ) -> Result<NodeName<'a>> {
        let recorded = hint.or_else(|| {
            v.node_rec
                .get(node as usize)
                .map(|r| r.name_bucket)
                .filter(|&b| b != NO_INDEX)
        });
        let bucket = match recorded {
            Some(b) => b,
            None => {
                let map = scan.get_or_insert_with(|| self.buckets_by_node(v));
                *map.get(&node)
                    .ok_or_else(|| self.corrupt(format!("node {node} has no name")))?
            }
        };
        let name = self.bucket(v, bucket)?;
        if name.node != node {
            return Err(self.corrupt(format!(
                "name bucket {bucket} names node {}, not node {node}",
                name.node
            )));
        }
        Ok(name)
    }

    /// The node of a port or `inst/pin` name, found through `NAME_HASH` (WS-REQ-0130).
    pub fn node_by_name(&self, name: &str) -> Result<Option<u32>> {
        let v = self.view()?;
        let buckets = v.name_hash.len() / BUCKET_WORDS;
        if buckets == 0 {
            return Ok(None);
        }
        let (mut slot, lo) = hash_slot(Hash64::of(name.as_bytes()).0, buckets - 1);
        for _ in 0..buckets {
            let stored = v.name_hash[slot * BUCKET_WORDS];
            if stored == 0 {
                return Ok(None);
            }
            if stored == lo {
                let n = self.bucket(&v, slot as u32)?;
                if n.joined() == name {
                    return Ok(Some(n.node));
                }
            }
            slot = (slot + 1) & (buckets - 1);
        }
        Ok(None)
    }

    fn check_corner(&self, corner: usize) -> Result<()> {
        if corner < self.meta.num_corners as usize {
            Ok(())
        } else {
            Err(Error::UnknownHandle {
                handle: format!("corner{}", corner + 1),
                reason: format!("the timing table has {} corners", self.meta.num_corners),
            })
        }
    }

    fn corner_name_in(&self, v: &View<'_>, corner: usize) -> Result<String> {
        let rec = v
            .corners
            .get(corner)
            .ok_or_else(|| self.corrupt(format!("corner {corner} is out of range")))?;
        Ok(self.string(v, rec.name_str)?.to_string())
    }

    /// The corner names, in corner order (WS-REQ-0131).
    pub fn corner_names(&self) -> Result<Vec<String>> {
        let v = self.view()?;
        (0..v.corners.len())
            .map(|c| self.corner_name_in(&v, c))
            .collect()
    }

    /// The clocks, in clock order (WS-REQ-0131).
    pub fn clocks(&self) -> Result<Vec<Clock>> {
        let v = self.view()?;
        v.clocks
            .iter()
            .map(|c| {
                Ok(Clock {
                    name: self.string(&v, c.name_str)?.to_string(),
                    period_ns: c.period_ns,
                    setup_uncertainty_ns: f64::from(c.uncertainty_setup_ns),
                })
            })
            .collect()
    }

    fn summary_row(&self, v: &View<'_>, s: &SummaryRec) -> Result<SummaryRow> {
        let corner = s.corner as usize;
        let wns = |value: f64, endpoints: u32| (endpoints > 0).then_some(value);
        Ok(SummaryRow {
            corner,
            corner_name: self.corner_name_in(v, corner)?,
            group: self.string(v, s.group_str)?.to_string(),
            setup_wns_ns: wns(s.setup_wns, s.setup_endpoints),
            setup_tns_ns: s.setup_tns,
            setup_violating: s.setup_violating,
            setup_endpoints: s.setup_endpoints,
            hold_wns_ns: wns(s.hold_wns, s.hold_endpoints),
            hold_tns_ns: s.hold_tns,
            hold_violating: s.hold_violating,
            hold_endpoints: s.hold_endpoints,
            max_transition_violating: s.max_trans_violating,
            max_capacitance_violating: s.max_cap_violating,
            total_endpoints: s.total_endpoints,
            unconstrained_endpoints: s.unconstrained_endpoints,
        })
    }

    /// Every row of `SUMMARY`, corner by corner (WS-REQ-0131).
    pub fn summary(&self) -> Result<Vec<SummaryRow>> {
        let v = self.view()?;
        v.summary.iter().map(|s| self.summary_row(&v, s)).collect()
    }

    /// The first `SUMMARY` row of `corner`, the one of all its endpoints (WS-REQ-0131).
    pub fn corner_summary(&self, corner: usize) -> Result<SummaryRow> {
        self.check_corner(corner)?;
        let v = self.view()?;
        let at = corner * self.meta.num_path_groups as usize;
        let Some(s) = v.summary.get(at) else {
            return Err(self.corrupt(format!("corner {corner} has no summary row")));
        };
        if s.corner as usize != corner {
            return Err(self.corrupt(format!(
                "summary row {at} belongs to corner {}, not corner {corner}",
                s.corner
            )));
        }
        self.summary_row(&v, s)
    }

    fn endpoint_in(
        &self,
        v: &View<'_>,
        index: usize,
        corner: usize,
        scan: &mut Option<HashMap<u32, u32>>,
    ) -> Result<Endpoint> {
        let rec = v
            .endpoints
            .get(index)
            .ok_or_else(|| self.corrupt(format!("endpoint {index} is out of range")))?;
        let kind = match rec.kind_flags & 0xFF {
            0 => EndpointKind::FlipFlop,
            1 => EndpointKind::Latch,
            2 => EndpointKind::OutputPort,
            3 => EndpointKind::RecoveryRemoval,
            4 => EndpointKind::ClockGate,
            k => return Err(self.corrupt(format!("endpoint {index} has kind {k}"))),
        };
        if (rec.kind_flags >> 8) & 0b11 == 0b11 {
            return Err(self.corrupt(format!("endpoint {index} has capture edge 3")));
        }
        let hint = (rec.kind_flags >> 10).checked_sub(1);
        let name = self.node_name(v, rec.data_node, hint, scan)?;
        let slack = corner
            .checked_mul(v.endpoints.len())
            .and_then(|base| v.ep_slack.get(base.checked_add(index)?))
            .ok_or_else(|| self.corrupt(format!("EP_SLACK lacks endpoint {index}")))?;
        let some = |x: f64| (!x.is_nan()).then_some(x);
        let rank = |r: u32| -> Result<Option<usize>> {
            if r == NO_INDEX {
                Ok(None)
            } else if r < self.meta.num_ranked_paths {
                Ok(Some(r as usize))
            } else {
                Err(self.corrupt(format!(
                    "endpoint {index} names ranked path {r} of {}",
                    self.meta.num_ranked_paths
                )))
            }
        };
        Ok(Endpoint {
            index,
            node: rec.data_node,
            name: name.joined(),
            instance: name.prefix.map(str::to_string),
            kind,
            setup_slack_ns: some(slack.setup_slack),
            hold_slack_ns: some(slack.hold_slack),
            worst_setup_path: rank(rec.worst_setup_rank)?,
            worst_hold_path: rank(rec.worst_hold_rank)?,
        })
    }

    /// The endpoints with their slacks in `corner`, in `ENDPOINTS` order (WS-REQ-0132).
    pub fn endpoints(&self, corner: usize) -> Result<Vec<Endpoint>> {
        self.check_corner(corner)?;
        let v = self.view()?;
        let mut scan = None;
        (0..v.endpoints.len())
            .map(|i| self.endpoint_in(&v, i, corner, &mut scan))
            .collect()
    }

    /// The endpoint rows of `corner` by ascending setup (or, with `hold`, hold) slack, as
    /// `EP_ORDER` lists them; the list is checked to be a permutation (WS-REQ-0132).
    pub fn endpoint_order(&self, corner: usize, hold: bool) -> Result<Vec<usize>> {
        self.check_corner(corner)?;
        let v = self.view()?;
        let n = v.endpoints.len();
        let base = (corner * 2 + usize::from(hold)) * n;
        let slice = v
            .ep_order
            .get(base..base + n)
            .ok_or_else(|| self.corrupt("EP_ORDER is shorter than its buckets"))?;
        let mut seen = vec![false; n];
        let mut out = Vec::with_capacity(n);
        for &e in slice {
            let e = e as usize;
            if e >= n || std::mem::replace(&mut seen[e], true) {
                return Err(self.corrupt(format!(
                    "EP_ORDER of corner {corner} is not a permutation of the endpoints"
                )));
            }
            out.push(e);
        }
        Ok(out)
    }

    /// The endpoint at `index` with its slacks in `corner` (WS-REQ-0132).
    pub fn endpoint(&self, index: usize, corner: usize) -> Result<Endpoint> {
        self.check_corner(corner)?;
        let v = self.view()?;
        if index >= v.endpoints.len() {
            return Err(Error::UnknownHandle {
                handle: format!("ep{}", index + 1),
                reason: format!("the timing table has {} endpoints", v.endpoints.len()),
            });
        }
        self.endpoint_in(&v, index, corner, &mut None)
    }

    fn rank_in(&self, v: &View<'_>, row: usize) -> Result<PathRank> {
        let Some(r) = v.path_rank.get(row) else {
            return Err(Error::UnknownHandle {
                handle: format!("path{}", row + 1),
                reason: format!("the timing table has {} ranked paths", v.path_rank.len()),
            });
        };
        let endpoint = usize::from(r.endpoint_lo) | (usize::from(r.corner_ep_hi >> 4) << 16);
        let corner = usize::from(r.corner_ep_hi & 0x0F);
        if endpoint >= v.endpoints.len() || corner >= v.corners.len() {
            return Err(self.corrupt(format!(
                "ranked path {row} names endpoint {endpoint} of corner {corner}"
            )));
        }
        let kind = match (r.flags >> 2) & 0b11 {
            0 => CheckKind::Setup,
            1 => CheckKind::Hold,
            2 => CheckKind::Recovery,
            _ => CheckKind::Removal,
        };
        if (r.flags & 1 == 1) != kind.is_hold_family() {
            return Err(self.corrupt(format!(
                "ranked path {row}: the early-check bit disagrees with its {} check",
                kind.as_str()
            )));
        }
        let slack_ns = f64::from(r.slack_ns);
        if !slack_ns.is_finite() {
            return Err(self.corrupt(format!("ranked path {row} has slack {slack_ns}")));
        }
        Ok(PathRank {
            row,
            kind,
            corner,
            endpoint,
            end_transition: if (r.flags >> 1) & 1 == 0 {
                Transition::Rise
            } else {
                Transition::Fall
            },
            slack_ns,
        })
    }

    /// Every rank record, in rank order (WS-REQ-0133).
    pub fn path_ranks(&self) -> Result<Vec<PathRank>> {
        let v = self.view()?;
        (0..v.path_rank.len())
            .map(|r| self.rank_in(&v, r))
            .collect()
    }

    /// The words of the chain of ranked path `row`, header included (WS-REQ-0129).
    fn chain_of<'a>(&self, v: &View<'a>, row: usize) -> Result<&'a [u32]> {
        let bounds = row
            .checked_add(1)
            .and_then(|next| Some((*v.chain_ptr.get(row)?, *v.chain_ptr.get(next)?)));
        let words = bounds.and_then(|(a, b)| v.chain.get(a as usize..b as usize));
        match words {
            Some(w) if w.len() >= CHAIN_HEADER_WORDS && w.len() % 2 == 0 => Ok(w),
            _ => Err(self.corrupt(format!(
                "the chain of ranked path {row} has a bad word range"
            ))),
        }
    }

    /// Ranked path `row`, the path of handle `path<row + 1>`, with its points (WS-REQ-0133,
    /// WS-REQ-0134). A row outside the ranked paths is `UNKNOWN_HANDLE`.
    pub fn path(&self, row: usize) -> Result<RankedPath> {
        let v = self.view()?;
        let rank = self.rank_in(&v, row)?;
        let mut scan = None;
        let endpoint = self.endpoint_in(&v, rank.endpoint, rank.corner, &mut scan)?;
        let chain = self.chain_of(&v, row)?;
        let steps: Vec<(u32, Transition, PointKind, u32)> = chain[CHAIN_HEADER_WORDS..]
            .chunks_exact(2)
            .map(|w| {
                let node = w[0] & 0x00FF_FFFF;
                let tr = if (w[0] >> 24) & 1 == 0 {
                    Transition::Rise
                } else {
                    Transition::Fall
                };
                (node, tr, point_kind(w[0] >> 25), w[1])
            })
            .collect();
        let point = |name: NodeName<'_>,
                     kind: PointKind,
                     transition: Transition,
                     arrival_ns: Option<f64>| PathPoint {
            node: name.node,
            name: name.joined(),
            instance: name.prefix.map(str::to_string),
            pin: name.leaf.to_string(),
            kind,
            transition,
            arrival_ns,
        };
        let mut points = Vec::with_capacity(steps.len());
        let (slack_ns, slack_exact, materialized) = if let Some(h) = v.paths.get(row) {
            if h.rank as usize != row || h.endpoint as usize != rank.endpoint {
                return Err(self.corrupt(format!(
                    "materialized path {row} records rank {} and endpoint {}",
                    h.rank, h.endpoint
                )));
            }
            let recs = v
                .points
                .get(h.point_begin as usize..h.point_end as usize)
                .ok_or_else(|| {
                    self.corrupt(format!("materialized path {row} has a bad point range"))
                })?;
            if !steps.is_empty() && steps.len() != recs.len() {
                return Err(self.corrupt(format!(
                    "materialized path {row} has {} points but its chain {} steps",
                    recs.len(),
                    steps.len()
                )));
            }
            for (k, p) in recs.iter().enumerate() {
                if p.transition > 1 || p.kind > 3 {
                    return Err(self.corrupt(format!(
                        "point {k} of materialized path {row} has transition {} and kind {}",
                        p.transition, p.kind
                    )));
                }
                // A chain step keeps the low 24 bits of its node (WS-REQ-0127).
                let hint = match steps.get(k) {
                    Some(&(node, _, _, bucket)) if node == p.node & 0x00FF_FFFF => Some(bucket),
                    Some(&(node, ..)) => {
                        return Err(self.corrupt(format!(
                            "point {k} of materialized path {row} is node {} but its chain \
                             step node {node}",
                            p.node
                        )));
                    }
                    None => None,
                };
                if p.node >= self.meta.num_nodes {
                    return Err(self.corrupt(format!(
                        "point {k} of materialized path {row} names node {}",
                        p.node
                    )));
                }
                let name = self.node_name(&v, p.node, hint, &mut scan)?;
                let tr = if p.transition == 0 {
                    Transition::Rise
                } else {
                    Transition::Fall
                };
                points.push(point(
                    name,
                    point_kind(u32::from(p.kind)),
                    tr,
                    Some(p.arrival),
                ));
            }
            (h.slack, true, true)
        } else {
            for &(node, tr, kind, bucket) in &steps {
                // The bucket holds the whole node; the step keeps its low 24 bits.
                let name = self.bucket(&v, bucket)?;
                if name.node & 0x00FF_FFFF != node {
                    return Err(self.corrupt(format!(
                        "a chain step of ranked path {row} is node {node}, its name bucket {bucket} \
                         names node {}",
                        name.node
                    )));
                }
                points.push(point(name, kind, tr, None));
            }
            (rank.slack_ns, false, false)
        };
        if !slack_ns.is_finite() {
            return Err(self.corrupt(format!("ranked path {row} has slack {slack_ns}")));
        }
        Ok(RankedPath {
            row,
            kind: rank.kind,
            corner: rank.corner,
            corner_name: self.corner_name_in(&v, rank.corner)?,
            endpoint: rank.endpoint,
            endpoint_name: endpoint.name,
            slack_ns,
            slack_exact,
            materialized,
            points,
        })
    }

    /// The required time and check value in ns that the chain header of ranked path `row`
    /// records (WS-REQ-0134).
    pub fn path_required(&self, row: usize) -> Result<(f64, f64)> {
        let v = self.view()?;
        self.rank_in(&v, row)?;
        let c = self.chain_of(&v, row)?;
        Ok((f64_from_words(c[0], c[1]), f64_from_words(c[2], c[3])))
    }

    /// The `SCOPE` section: a JSON object (WS-REQ-0131).
    pub fn scope(&self) -> Result<Map<String, Value>> {
        let v = self.view()?;
        match serde_json::from_slice::<Value>(v.scope) {
            Ok(Value::Object(m)) => Ok(m),
            Ok(_) => Err(self.corrupt("SCOPE is not a JSON object")),
            Err(e) => Err(self.corrupt(format!("SCOPE is not JSON: {e}"))),
        }
    }
}
