// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! [`StaTableBuilder`]: a small writer of valid `STA` v1 tables (WS-REQ-0135).
//!
//! The builder is for tools and tests that need a timing table without a timing engine: it
//! takes ports, instance pins, endpoints with their slacks and ranked paths with their points,
//! and computes everything else of the format (string table, name and instance hash tables,
//! endpoint orders, path ranks and chains, materialized paths, summary rows and scope) the way
//! the specification describes. It analyzes nothing: every slack and arrival is the caller's.

use std::collections::HashMap;

use serde_json::json;

use super::layout::{
    BUCKET_WORDS, ClockRec, CornerRec, EndpointRec, EpSlackRec, MetaRec, NodeRec, PathHeaderRec,
    PathPointRec, PathRankRec, SummaryRec, TagRec, bucket_count, canon, f64_to_words, hash_slot,
    section,
};
use super::{
    CheckKind, EndpointKind, FORMAT_VERSION, KIND, NO_INDEX, OVERALL_GROUP, PointKind, StoreMode,
    TABLE_NAME, TABLE_PATH, Transition,
};
use crate::error::{Error, Result};
use crate::hash::Hash64;
use crate::rbin::RbinWriter;
use crate::schema::Upstream;
use crate::workspace::Txn;

/// A node of a table being built: a port or an instance pin (WS-REQ-0135).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeRef(usize);

/// A ranked path to add (WS-REQ-0135).
#[derive(Debug, Clone, PartialEq)]
pub struct PathSpec {
    /// The corner index, as [`StaTableBuilder::corner`] returned it.
    pub corner: usize,
    /// The check.
    pub kind: CheckKind,
    /// The slack in ns.
    pub slack_ns: f64,
    /// The required time in ns.
    pub required_ns: f64,
    /// The points from the startpoint to the endpoint: node, transition, what reaches the
    /// point, arrival in ns. The last point is an endpoint ([`StaTableBuilder::endpoint`]).
    pub points: Vec<(NodeRef, Transition, PointKind, f64)>,
}

#[derive(Debug, Clone, PartialEq)]
enum NodeSpec {
    Port(String),
    Pin { instance: usize, pin: String },
}

#[derive(Debug, Clone, PartialEq)]
struct EndpointSpec {
    node: NodeRef,
    kind: EndpointKind,
    /// Per corner: setup and hold slack.
    slack: Vec<(Option<f64>, Option<f64>)>,
}

/// A writer of minimal valid `STA` v1 tables (WS-REQ-0135).
///
/// ```
/// use rusteda_workspace::sta::{
///     CheckKind, EndpointKind, PathSpec, PointKind, StaTable, StaTableBuilder, StoreMode,
///     Transition,
/// };
///
/// let mut b = StaTableBuilder::new(StoreMode::Summary);
/// let tt = b.corner("tt");
/// b.clock("clk", 1.0, 0.0);
/// let a = b.port("a");
/// let u1_a = b.pin("u1", "A");
/// let u1_y = b.pin("u1", "Y");
/// let y = b.port("y");
/// b.endpoint(y, EndpointKind::OutputPort);
/// b.endpoint_slack(y, tt, Some(-0.25), None);
/// b.path(PathSpec {
///     corner: tt,
///     kind: CheckKind::Setup,
///     slack_ns: -0.25,
///     required_ns: 0.75,
///     points: vec![
///         (a, Transition::Rise, PointKind::Port, 0.0),
///         (u1_a, Transition::Rise, PointKind::Net, 0.0),
///         (u1_y, Transition::Fall, PointKind::Cell, 0.6),
///         (y, Transition::Fall, PointKind::Net, 1.0),
///     ],
/// });
/// let table = StaTable::from_bytes(b.to_bytes(&Default::default())?)?;
/// assert_eq!(table.path(0)?.instances(), ["u1"]);
/// # Ok::<(), rusteda_workspace::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct StaTableBuilder {
    mode: StoreMode,
    top: String,
    corners: Vec<String>,
    clocks: Vec<(String, f64, f64)>,
    nodes: Vec<NodeSpec>,
    node_ids: HashMap<(Option<usize>, String), NodeRef>,
    instances: Vec<(String, String)>,
    instance_ids: HashMap<String, usize>,
    endpoints: Vec<EndpointSpec>,
    paths: Vec<PathSpec>,
    top_k: Option<usize>,
    /// The first call that named an unknown endpoint or corner; building fails with it.
    misuse: Option<String>,
}

/// The string table under construction: string 0 is empty, equal strings share an id.
#[derive(Default)]
struct Strings {
    bytes: Vec<u8>,
    offs: Vec<u32>,
    ids: HashMap<String, u32>,
}

impl Strings {
    fn new() -> Strings {
        let mut s = Strings {
            bytes: Vec::new(),
            offs: vec![0],
            ids: HashMap::new(),
        };
        s.intern("");
        s
    }

    fn intern(&mut self, text: &str) -> u32 {
        if let Some(&id) = self.ids.get(text) {
            return id;
        }
        let id = (self.offs.len() - 1) as u32;
        self.bytes.extend_from_slice(text.as_bytes());
        self.offs.push(self.bytes.len() as u32);
        self.ids.insert(text.to_string(), id);
        id
    }
}

/// Inserts `hash` into an open-addressed table of 4-word buckets and returns the bucket.
fn insert(words: &mut [u32], hash: u64, rest: [u32; 3]) -> u32 {
    let buckets = words.len() / BUCKET_WORDS;
    let (mut slot, lo) = hash_slot(hash, buckets - 1);
    while words[slot * BUCKET_WORDS] != 0 {
        slot = (slot + 1) & (buckets - 1);
    }
    let at = slot * BUCKET_WORDS;
    words[at] = lo;
    words[at + 1..at + BUCKET_WORDS].copy_from_slice(&rest);
    slot as u32
}

/// `-0.0` as `+0.0` for an `f32` (WS-REQ-0135).
fn canon32(x: f64) -> f32 {
    let v = canon(x) as f32;
    if v == 0.0 { 0.0 } else { v }
}

/// The kind code of an endpoint in `ENDPOINTS` (WS-REQ-0132).
fn endpoint_code(k: EndpointKind) -> u32 {
    match k {
        EndpointKind::FlipFlop => 0,
        EndpointKind::Latch => 1,
        EndpointKind::OutputPort => 2,
        EndpointKind::RecoveryRemoval => 3,
        EndpointKind::ClockGate => 4,
    }
}

/// The two-bit code of a path point kind (WS-REQ-0134).
fn point_code(k: PointKind) -> u32 {
    match k {
        PointKind::Port => 0,
        PointKind::Net => 1,
        PointKind::Cell => 2,
        PointKind::ClockLaunch => 3,
    }
}

fn internal(message: impl Into<String>) -> Error {
    Error::internal(format!("StaTableBuilder: {}", message.into()))
}

/// The sort key of an `EP_ORDER` bucket: ascending slack, NaN last, then row.
fn order(slacks: &[f64]) -> Vec<u32> {
    let mut rows: Vec<u32> = (0..slacks.len() as u32).collect();
    rows.sort_by(|&a, &b| {
        let (x, y) = (slacks[a as usize], slacks[b as usize]);
        match (x.is_nan(), y.is_nan()) {
            (false, false) => x.total_cmp(&y),
            (a_nan, b_nan) => a_nan.cmp(&b_nan),
        }
        .then(a.cmp(&b))
    });
    rows
}

/// The tables computed from the inputs, before they are written.
struct Built {
    strings: Strings,
    meta: MetaRec,
    corners: Vec<CornerRec>,
    clocks: Vec<ClockRec>,
    tags: Vec<TagRec>,
    node_rec: Vec<NodeRec>,
    tag_extra_ptr: Vec<u32>,
    endpoints: Vec<EndpointRec>,
    ep_slack: Vec<EpSlackRec>,
    ep_order: Vec<u32>,
    path_rank: Vec<PathRankRec>,
    chain_ptr: Vec<u64>,
    chain: Vec<u32>,
    paths: Vec<PathHeaderRec>,
    points: Vec<PathPointRec>,
    name_hash: Vec<u32>,
    inst_hash: Vec<u32>,
    summary: Vec<SummaryRec>,
    scope: Vec<u8>,
}

impl StaTableBuilder {
    /// An empty table of `mode`.
    pub fn new(mode: StoreMode) -> StaTableBuilder {
        StaTableBuilder {
            mode,
            top: String::new(),
            corners: Vec::new(),
            clocks: Vec::new(),
            nodes: Vec::new(),
            node_ids: HashMap::new(),
            instances: Vec::new(),
            instance_ids: HashMap::new(),
            endpoints: Vec::new(),
            paths: Vec::new(),
            top_k: None,
            misuse: None,
        }
    }

    /// The top module recorded in `SCOPE`.
    pub fn top(&mut self, name: &str) -> &mut Self {
        self.top = name.to_string();
        self
    }

    /// Adds a corner and returns its index; a name given before returns the same index.
    pub fn corner(&mut self, name: &str) -> usize {
        if let Some(i) = self.corners.iter().position(|c| c == name) {
            return i;
        }
        self.corners.push(name.to_string());
        for e in &mut self.endpoints {
            e.slack.push((None, None));
        }
        self.corners.len() - 1
    }

    /// Adds a clock with its period and setup uncertainty in ns.
    pub fn clock(&mut self, name: &str, period_ns: f64, setup_uncertainty_ns: f64) -> &mut Self {
        self.clocks
            .push((name.to_string(), period_ns, setup_uncertainty_ns));
        self
    }

    /// The node of port (bit) `name`.
    pub fn port(&mut self, name: &str) -> NodeRef {
        let key = (None, name.to_string());
        if let Some(&n) = self.node_ids.get(&key) {
            return n;
        }
        let n = NodeRef(self.nodes.len());
        self.nodes.push(NodeSpec::Port(name.to_string()));
        self.node_ids.insert(key, n);
        n
    }

    /// Declares instance `name` of `cell`; a later call changes the cell.
    pub fn instance(&mut self, name: &str, cell: &str) -> &mut Self {
        let i = self.instance_index(name);
        self.instances[i].1 = cell.to_string();
        self
    }

    fn instance_index(&mut self, name: &str) -> usize {
        if let Some(&i) = self.instance_ids.get(name) {
            return i;
        }
        self.instances.push((name.to_string(), String::new()));
        self.instance_ids
            .insert(name.to_string(), self.instances.len() - 1);
        self.instances.len() - 1
    }

    /// The node of pin `pin` of instance `instance`, which is declared with an empty cell name
    /// when it is new.
    pub fn pin(&mut self, instance: &str, pin: &str) -> NodeRef {
        let i = self.instance_index(instance);
        let key = (Some(i), pin.to_string());
        if let Some(&n) = self.node_ids.get(&key) {
            return n;
        }
        let n = NodeRef(self.nodes.len());
        self.nodes.push(NodeSpec::Pin {
            instance: i,
            pin: pin.to_string(),
        });
        self.node_ids.insert(key, n);
        n
    }

    /// Makes `node` a checked endpoint of `kind`; its slacks are absent until
    /// [`StaTableBuilder::endpoint_slack`] sets them. A second call changes the kind.
    pub fn endpoint(&mut self, node: NodeRef, kind: EndpointKind) -> &mut Self {
        if let Some(e) = self.endpoints.iter_mut().find(|e| e.node == node) {
            e.kind = kind;
        } else {
            self.endpoints.push(EndpointSpec {
                node,
                kind,
                slack: vec![(None, None); self.corners.len()],
            });
        }
        self
    }

    /// Sets the setup and hold slack in ns of endpoint `node` in `corner`.
    pub fn endpoint_slack(
        &mut self,
        node: NodeRef,
        corner: usize,
        setup_ns: Option<f64>,
        hold_ns: Option<f64>,
    ) -> &mut Self {
        let slot = self
            .endpoints
            .iter_mut()
            .find(|e| e.node == node)
            .and_then(|e| e.slack.get_mut(corner));
        match slot {
            Some(s) => *s = (setup_ns, hold_ns),
            None => {
                self.misuse.get_or_insert_with(|| {
                    format!("endpoint_slack names node {} or corner {corner}, which are not an endpoint and a corner", node.0)
                });
            }
        }
        self
    }

    /// Adds a ranked path; the paths are ranked when the table is built.
    pub fn path(&mut self, spec: PathSpec) -> &mut Self {
        self.paths.push(spec);
        self
    }

    /// Materializes only the first `k` ranked paths (default: all of them).
    pub fn top_k(&mut self, k: usize) -> &mut Self {
        self.top_k = Some(k);
        self
    }

    /// The table bytes, recording `upstream` in the rbin header (WS-REQ-0038, WS-REQ-0135).
    pub fn to_bytes(&self, upstream: &Upstream) -> Result<Vec<u8>> {
        let built = self.build()?;
        let mut w = built.writer()?;
        w.upstream_tables(upstream)?;
        Ok(w.to_bytes())
    }

    /// Adds the table to `txn` as table `sta` (`sta.rbin`) computed from `upstream`, with the
    /// summary values and the `timing` status of the timing table (WS-REQ-0135).
    pub fn stage(&self, txn: &mut Txn, upstream: Upstream) -> Result<Hash64> {
        let built = self.build()?;
        let w = built.writer()?;
        let hash = txn.add_rbin(TABLE_NAME, TABLE_PATH, &w, upstream)?;
        let mut setup_wns: Option<f64> = None;
        let mut hold_wns: Option<f64> = None;
        let (mut setup_tns, mut hold_tns) = (0.0f64, 0.0f64);
        let (mut setup_viol, mut hold_viol) = (0u32, 0u32);
        for s in &built.summary {
            if s.setup_endpoints > 0 {
                setup_wns = Some(setup_wns.map_or(s.setup_wns, |w| w.min(s.setup_wns)));
            }
            if s.hold_endpoints > 0 {
                hold_wns = Some(hold_wns.map_or(s.hold_wns, |w| w.min(s.hold_wns)));
            }
            setup_tns = setup_tns.min(s.setup_tns);
            hold_tns = hold_tns.min(s.hold_tns);
            setup_viol = setup_viol.max(s.setup_violating);
            hold_viol = hold_viol.max(s.hold_violating);
        }
        let values = [
            ("setup_wns_ns", json!(canon(setup_wns.unwrap_or(0.0)))),
            ("setup_tns_ns", json!(canon(setup_tns))),
            ("setup_violating_endpoints", json!(setup_viol)),
            ("hold_wns_ns", json!(canon(hold_wns.unwrap_or(0.0)))),
            ("hold_tns_ns", json!(canon(hold_tns))),
            ("hold_violating_endpoints", json!(hold_viol)),
            // The builder checks no design rule: its counts are unknown, not zero
            // (WS-REQ-0137).
            ("max_trans_violating", serde_json::Value::Null),
            ("max_cap_violating", serde_json::Value::Null),
            ("drv_status", json!("unchecked")),
            ("total_endpoints", json!(built.meta.num_endpoints)),
            ("unconstrained_endpoints", json!(0)),
            ("corners", json!(built.meta.num_corners)),
        ];
        for (k, v) in values {
            txn.set_summary(TABLE_NAME, k, v)?;
        }
        let violated = setup_viol > 0 || hold_viol > 0;
        txn.set_status(
            TABLE_NAME,
            "timing",
            if violated { "violated" } else { "met" },
        )?;
        Ok(hash)
    }

    /// The node ids: ports first in declaration order, then the pins of each instance, in
    /// the order the instances and their pins were declared (WS-REQ-0135).
    fn number(&self) -> (Vec<u32>, Vec<(u32, u32)>) {
        let mut id = vec![0u32; self.nodes.len()];
        let mut next = 0u32;
        for (i, n) in self.nodes.iter().enumerate() {
            if matches!(n, NodeSpec::Port(_)) {
                id[i] = next;
                next += 1;
            }
        }
        let mut ranges = vec![(0u32, 0u32); self.instances.len()];
        let mut pins_of: Vec<Vec<usize>> = vec![Vec::new(); self.instances.len()];
        for (i, n) in self.nodes.iter().enumerate() {
            if let NodeSpec::Pin { instance, .. } = n {
                pins_of[*instance].push(i);
            }
        }
        for (inst, pins) in pins_of.iter().enumerate() {
            ranges[inst] = (next, pins.len() as u32);
            for &p in pins {
                id[p] = next;
                next += 1;
            }
        }
        (id, ranges)
    }

    fn build(&self) -> Result<Built> {
        if let Some(m) = &self.misuse {
            return Err(internal(m.clone()));
        }
        if self.corners.is_empty() {
            return Err(internal("a timing table has at least one corner"));
        }
        if self.corners.len() > 16 {
            return Err(internal("a timing table addresses at most 16 corners"));
        }
        let n_nodes = self.nodes.len();
        if n_nodes >= NO_INDEX as usize {
            return Err(internal(
                "a timing table addresses fewer than 2^32 - 1 nodes",
            ));
        }
        let (node_id, ranges) = self.number();
        let mut by_id = vec![0usize; n_nodes];
        for (spec, &id) in node_id.iter().enumerate() {
            by_id[id as usize] = spec;
        }
        let n_ports = self
            .nodes
            .iter()
            .filter(|n| matches!(n, NodeSpec::Port(_)))
            .count();
        let mut strings = Strings::new();
        let backend_str = strings.intern("builder");
        let group_str = strings.intern(OVERALL_GROUP);
        let corner_strs: Vec<u32> = self.corners.iter().map(|c| strings.intern(c)).collect();
        let clock_strs: Vec<u32> = self.clocks.iter().map(|c| strings.intern(&c.0)).collect();
        let cell_strs: Vec<u32> = self
            .instances
            .iter()
            .map(|(_, cell)| strings.intern(cell))
            .collect();
        if cell_strs.iter().any(|&s| s > 0x000F_FFFF) {
            return Err(internal("INST_HASH addresses cell names below string 2^20"));
        }
        let inst_strs: Vec<u32> = self
            .instances
            .iter()
            .map(|(name, _)| strings.intern(name))
            .collect();

        // NAME_HASH and INST_HASH (WS-REQ-0130).
        let mut name_hash = vec![0u32; bucket_count(n_nodes) * BUCKET_WORDS];
        let mut bucket_of = vec![NO_INDEX; n_nodes];
        for &spec in &by_id {
            let (name, prefix, leaf) = match &self.nodes[spec] {
                NodeSpec::Port(p) => (p.clone(), NO_INDEX, strings.intern(p)),
                NodeSpec::Pin { instance, pin } => (
                    format!("{}/{pin}", self.instances[*instance].0),
                    inst_strs[*instance],
                    strings.intern(pin),
                ),
            };
            let id = node_id[spec];
            bucket_of[id as usize] = insert(
                &mut name_hash,
                Hash64::of(name.as_bytes()).0,
                [id, prefix, leaf],
            );
        }
        let mut inst_hash = vec![0u32; bucket_count(self.instances.len()) * BUCKET_WORDS];
        for (i, (name, _)) in self.instances.iter().enumerate() {
            let (begin, count) = ranges[i];
            if count > 0x0FFF {
                return Err(internal("INST_HASH packs at most 4095 pins per instance"));
            }
            insert(
                &mut inst_hash,
                Hash64::of(name.as_bytes()).0,
                [inst_strs[i], cell_strs[i] | (count << 20), begin],
            );
        }

        // Endpoints in node order (WS-REQ-0132).
        let mut eps: Vec<&EndpointSpec> = self.endpoints.iter().collect();
        eps.sort_by_key(|e| node_id[e.node.0]);
        if eps.len() >= NO_INDEX as usize {
            return Err(internal(
                "a timing table addresses fewer than 2^32 - 1 endpoints",
            ));
        }
        let ep_of: HashMap<NodeRef, usize> =
            eps.iter().enumerate().map(|(i, e)| (e.node, i)).collect();
        let n_corners = self.corners.len();
        let n_eps = eps.len();
        let mut ep_slack = Vec::with_capacity(n_corners * n_eps);
        for c in 0..n_corners {
            for e in &eps {
                let (s, h) = e.slack[c];
                ep_slack.push(EpSlackRec {
                    setup_slack: canon(s.unwrap_or(f64::NAN)),
                    hold_slack: canon(h.unwrap_or(f64::NAN)),
                });
            }
        }
        let mut ep_order = Vec::with_capacity(2 * n_corners * n_eps);
        for c in 0..n_corners {
            let row = &ep_slack[c * n_eps..(c + 1) * n_eps];
            let setup: Vec<f64> = row.iter().map(|s| s.setup_slack).collect();
            let hold: Vec<f64> = row.iter().map(|s| s.hold_slack).collect();
            ep_order.extend(order(&setup));
            ep_order.extend(order(&hold));
        }

        // Ranked paths: setup family first, then by slack, corner, endpoint and end
        // transition, ties in the order given (WS-REQ-0135).
        let mut ranked: Vec<(usize, &PathSpec)> = Vec::with_capacity(self.paths.len());
        for p in &self.paths {
            let Some(&(last, ..)) = p.points.last() else {
                return Err(internal("a path has at least one point"));
            };
            let Some(&ep) = ep_of.get(&last) else {
                return Err(internal("a path ends at an endpoint"));
            };
            if p.corner >= n_corners
                || !p.slack_ns.is_finite()
                || !p.required_ns.is_finite()
                || p.points
                    .iter()
                    .any(|q| !q.3.is_finite() || q.0.0 >= n_nodes)
            {
                return Err(internal(
                    "a path has a known corner and nodes and finite times",
                ));
            }
            ranked.push((ep, p));
        }
        let tr_bit = |t: Transition| u8::from(t == Transition::Fall);
        ranked.sort_by(|(ea, a), (eb, b)| {
            a.kind
                .is_hold_family()
                .cmp(&b.kind.is_hold_family())
                .then(a.slack_ns.total_cmp(&b.slack_ns))
                .then(a.corner.cmp(&b.corner))
                .then(ea.cmp(eb))
                .then(
                    tr_bit(a.points[a.points.len() - 1].1)
                        .cmp(&tr_bit(b.points[b.points.len() - 1].1)),
                )
        });
        let top_k = self.top_k.unwrap_or(ranked.len()).min(ranked.len());
        let mut endpoints: Vec<EndpointRec> = eps
            .iter()
            .map(|e| {
                let id = node_id[e.node.0];
                let bucket = bucket_of[id as usize];
                EndpointRec {
                    data_node: id,
                    clock_node: NO_INDEX,
                    capture_clock: if self.clocks.is_empty() { NO_INDEX } else { 0 },
                    path_group_str: group_str,
                    check_arc: NO_INDEX,
                    kind_flags: endpoint_code(e.kind) | (2 << 8),
                    worst_setup_rank: NO_INDEX,
                    worst_hold_rank: NO_INDEX,
                    name_bucket_plus1: if bucket == NO_INDEX { 0 } else { bucket + 1 },
                }
            })
            .collect();
        let mut path_rank = Vec::with_capacity(ranked.len());
        let mut chain_ptr = vec![0u64];
        let mut chain = Vec::new();
        let mut paths = Vec::with_capacity(top_k);
        let mut points = Vec::new();
        for (row, (ep, p)) in ranked.iter().enumerate() {
            let kind_bits: u8 = match p.kind {
                CheckKind::Setup => 0,
                CheckKind::Hold => 1,
                CheckKind::Recovery => 2,
                CheckKind::Removal => 3,
            };
            let end = p.points[p.points.len() - 1];
            path_rank.push(PathRankRec {
                slack_ns: canon32(p.slack_ns),
                endpoint: *ep as u32,
                corner: p.corner as u16,
                flags: u8::from(p.kind.is_hold_family())
                    | (tr_bit(end.1) << 1)
                    | (kind_bits << 2)
                    | (u8::from(row < top_k) << 7),
                pad: 0,
            });
            let worst = if p.kind.is_hold_family() {
                &mut endpoints[*ep].worst_hold_rank
            } else {
                &mut endpoints[*ep].worst_setup_rank
            };
            if *worst == NO_INDEX {
                *worst = row as u32;
            }
            chain.extend(f64_to_words(canon(p.required_ns)));
            chain.extend(f64_to_words(0.0));
            for &(n, tr, kind, _) in &p.points {
                let id = node_id[n.0];
                chain.push(id);
                chain.push(u32::from(tr_bit(tr)) | (point_code(kind) << 1));
                chain.push(bucket_of[id as usize]);
            }
            chain_ptr.push(chain.len() as u64);
            if row < top_k {
                let begin = points.len() as u32;
                let mut prev = 0.0;
                for (k, &(n, tr, kind, arrival)) in p.points.iter().enumerate() {
                    points.push(PathPointRec {
                        arrival: canon(arrival),
                        incr_delay: canon(if k == 0 { arrival } else { arrival - prev }),
                        slew: 0.0,
                        node: node_id[n.0],
                        transition: tr_bit(tr),
                        kind: point_code(kind) as u8,
                        pad: 0,
                    });
                    prev = arrival;
                }
                paths.push(PathHeaderRec {
                    slack: canon(p.slack_ns),
                    arrival: canon(end.3),
                    required: canon(p.required_ns),
                    check_value: 0.0,
                    rank: row as u32,
                    endpoint: *ep as u32,
                    launch_clock: if self.clocks.is_empty() { NO_INDEX } else { 0 },
                    capture_clock: if self.clocks.is_empty() { NO_INDEX } else { 0 },
                    point_begin: begin,
                    point_end: points.len() as u32,
                });
            }
        }

        // NODE_REC in the full mode: absent values, path arrivals under the one tag.
        let (node_rec, tag_extra_ptr) = match self.mode {
            StoreMode::Summary => (Vec::new(), Vec::new()),
            StoreMode::Full => {
                let absent = |bucket: u32| NodeRec {
                    slew: [[0.0; 2]; 2],
                    required: [[f64::NEG_INFINITY, f64::INFINITY]; 2],
                    slack: [[f64::INFINITY; 2]; 2],
                    tag: NO_INDEX,
                    name_bucket: bucket,
                    arrival: [[f64::INFINITY, f64::NEG_INFINITY]; 2],
                    pred: [[NO_INDEX; 2]; 2],
                };
                let mut recs = Vec::with_capacity(n_corners * n_nodes);
                for _ in 0..n_corners {
                    recs.extend(bucket_of.iter().map(|&b| absent(b)));
                }
                for p in &self.paths {
                    let late = usize::from(!p.kind.is_hold_family());
                    for &(n, tr, _, arrival) in &p.points {
                        let r = &mut recs[p.corner * n_nodes + node_id[n.0] as usize];
                        r.tag = 0;
                        let slot = &mut r.arrival[usize::from(tr_bit(tr))][late];
                        *slot = if late == 1 {
                            slot.max(arrival)
                        } else {
                            slot.min(arrival)
                        };
                    }
                }
                (recs, vec![0u32; n_corners * n_nodes + 1])
            }
        };

        // SUMMARY: one row per corner, of all endpoints (WS-REQ-0131).
        let mut summary = Vec::with_capacity(n_corners);
        for c in 0..n_corners {
            let mut s = SummaryRec {
                corner: c as u32,
                group_str,
                total_endpoints: n_eps as u32,
                ..SummaryRec::default()
            };
            let (mut sw, mut hw) = (f64::INFINITY, f64::INFINITY);
            for e in &ep_slack[c * n_eps..(c + 1) * n_eps] {
                if e.setup_slack.is_finite() {
                    s.setup_endpoints += 1;
                    sw = sw.min(e.setup_slack);
                    if e.setup_slack < 0.0 {
                        s.setup_tns += e.setup_slack;
                        s.setup_violating += 1;
                    }
                }
                if e.hold_slack.is_finite() {
                    s.hold_endpoints += 1;
                    hw = hw.min(e.hold_slack);
                    if e.hold_slack < 0.0 {
                        s.hold_tns += e.hold_slack;
                        s.hold_violating += 1;
                    }
                }
            }
            s.setup_wns = canon(if sw.is_finite() { sw } else { 0.0 });
            s.hold_wns = canon(if hw.is_finite() { hw } else { 0.0 });
            s.setup_tns = canon(s.setup_tns);
            s.hold_tns = canon(s.hold_tns);
            summary.push(s);
        }

        let scope = serde_json::to_vec(&json!({
            "top": self.top,
            "corners": self.corners,
            "wire_model": "none",
            "clock_model": "ideal",
            "cppr": "none",
            "backend": "builder",
            "precision": "f64",
            "store_mode": self.mode.as_str(),
            "exceptions_applied": [],
            "completeness": {
                "analyzed_endpoints": n_eps,
                "unconstrained_endpoints": 0,
                "unconstrained_by_reason": {},
                "disabled_arcs_by_reason": {},
                "unanalyzed_arcs_by_type": {},
            },
        }))
        .map_err(|e| internal(format!("scope JSON: {e}")))?;

        let clock_id = if self.clocks.is_empty() { NO_INDEX } else { 0 };
        let meta = MetaRec {
            num_nodes: n_nodes as u32,
            num_port_bits: n_ports as u32,
            num_cells: self.instances.len() as u32,
            num_corners: n_corners as u32,
            num_clocks: self.clocks.len() as u32,
            num_tags: 1,
            num_endpoints: n_eps as u32,
            num_ranked_paths: path_rank.len() as u32,
            num_top_k_paths: top_k as u32,
            num_path_groups: 1,
            store_mode: match self.mode {
                StoreMode::Full => 0,
                StoreMode::Summary => 1,
            },
            backend_str,
            ..MetaRec::default()
        };
        Ok(Built {
            meta,
            corners: corner_strs
                .iter()
                .map(|&name_str| CornerRec {
                    name_str,
                    ..CornerRec::default()
                })
                .collect(),
            clocks: self
                .clocks
                .iter()
                .zip(&clock_strs)
                .map(|((_, period, unc), &name_str)| ClockRec {
                    name_str,
                    source_node: NO_INDEX,
                    master_clock: NO_INDEX,
                    flags: 1,
                    period_ns: canon(*period),
                    rise_edge_ns: 0.0,
                    fall_edge_ns: canon(*period / 2.0),
                    uncertainty_setup_ns: canon32(*unc),
                    uncertainty_hold_ns: 0.0,
                })
                .collect(),
            tags: vec![TagRec {
                clock_id,
                launch_source_node: NO_INDEX,
                path_group_str: group_str,
                ..TagRec::default()
            }],
            node_rec,
            tag_extra_ptr,
            endpoints,
            ep_slack,
            ep_order,
            path_rank,
            chain_ptr,
            chain,
            paths,
            points,
            name_hash,
            inst_hash,
            summary,
            scope,
            strings,
        })
    }
}

impl Built {
    /// The 24 sections in id order (WS-REQ-0126).
    fn writer(&self) -> Result<RbinWriter<'_>> {
        let mut w = RbinWriter::new(KIND, FORMAT_VERSION)?;
        let empty: &[u8] = &[];
        w.section_bytes(section::STR_BYTES, 1, self.strings.bytes.as_slice())?;
        w.section(section::STR_OFFS, &self.strings.offs)?;
        w.section(section::META, std::slice::from_ref(&self.meta))?;
        w.section(section::CORNERS, &self.corners)?;
        w.section(section::CLOCKS, &self.clocks)?;
        w.section(section::TAGS, &self.tags)?;
        w.section_bytes(section::DISABLED_ARCS, 16, empty)?;
        w.section(section::NODE_REC, &self.node_rec)?;
        w.section(section::TAG_EXTRA_PTR, &self.tag_extra_ptr)?;
        w.section_bytes(section::TAG_EXTRA, 56, empty)?;
        w.section(section::ENDPOINTS, &self.endpoints)?;
        w.section(section::EP_SLACK, &self.ep_slack)?;
        w.section(section::EP_ORDER, &self.ep_order)?;
        w.section(section::PATH_RANK, &self.path_rank)?;
        w.section(section::PATH_CHAIN_PTR, &self.chain_ptr)?;
        w.section(section::PATH_CHAIN, &self.chain)?;
        w.section(section::PATHS, &self.paths)?;
        w.section(section::PATH_POINTS, &self.points)?;
        w.section(section::NAME_HASH, &self.name_hash)?;
        w.section(section::INST_HASH, &self.inst_hash)?;
        w.section(section::SUMMARY, &self.summary)?;
        w.section_bytes(section::UNCONSTRAINED, 12, empty)?;
        w.section_bytes(section::NOTES, 16, empty)?;
        w.section_bytes(section::SCOPE, 1, self.scope.as_slice())?;
        Ok(w)
    }
}
