// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The `NETLIST` v1 (`netlist.rbin`) zero-copy codec over [`FrozenNetlist`] and workspace
//! netlist table helpers (contract §5, `WS-REQ-0120` to `WS-REQ-0125`).
//!
//! ## Container layout (`WS-REQ-0120`, `WS-REQ-0121`)
//!
//! A workspace netlist table (`revisions/rev<N>/netlist.rbin`, table name [`TABLE_NAME`]) is an
//! [`Rbin`] container of kind [`KIND`] (`"NETLIST"`) and format version [`FORMAT_VERSION`] (`1`,
//! range [`VERSIONS`]). It writes the 18 sections of [`FrozenNetlist`] (`section::STR_BYTES = 1`
//! through `section::NET_PINS = 18`) in ascending section ID order with the element sizes of
//! [`section::elem_size`], and records the table's upstream map in the `rbin` header per
//! `WS-REQ-0038`.
//!
//! ## Zero-copy decoding (`WS-REQ-0122`, `WS-REQ-0123`)
//!
//! [`decode`] verifies the container kind, format version, `xxh3-64` content hash, and the
//! presence and element sizes of all 18 sections, then constructs a [`FrozenNetlist`] whose 18
//! fields are [`Cow::Borrowed`] slices pointing directly into the mapped or 8-byte aligned
//! [`Rbin`] buffer (zero-copy `bytemuck` casts). Finally, [`FrozenNetlist::validate`] runs on
//! the borrowed tables and maps any [`FrozenError`] to `CORRUPT_TABLE`.

use std::borrow::Cow;
use std::ops::RangeInclusive;
use std::path::Path;

pub use rusteda_core::netlist::{
    ANONYMOUS, AssignRec, CONST0, CONST1, FORMAT_VERSION, FrozenError, FrozenNetlist, GateNetlist,
    InstRec, NetRec, PortRec, SCALAR, module_entry, section,
};

use crate::error::{Corruption, Error, Result};
use crate::hash::Hash64;
use crate::rbin::{Rbin, RbinWriter};
use crate::schema::Upstream;
use crate::workspace::{Revision, Txn, Workspace};

/// The `rbin` container kind for netlist tables (`WS-REQ-0120`).
pub const KIND: &str = "NETLIST";

/// The range of `NETLIST` format versions supported by this build (`WS-REQ-0120`).
pub const VERSIONS: RangeInclusive<u32> = FORMAT_VERSION..=FORMAT_VERSION;

/// The canonical workspace table name for a revision's netlist (`WS-REQ-0120`).
pub const TABLE_NAME: &str = "netlist";

/// The canonical filename inside a revision directory for the netlist table (`WS-REQ-0120`).
pub const TABLE_PATH: &str = "netlist.rbin";

/// Builds an [`RbinWriter`] for `netlist` with kind [`KIND`], format version [`FORMAT_VERSION`],
/// and all 18 sections in ascending ID order (`WS-REQ-0120`, `WS-REQ-0121`).
pub fn writer<'a>(netlist: &'a FrozenNetlist<'_>) -> Result<RbinWriter<'a>> {
    let mut w = RbinWriter::new(KIND, FORMAT_VERSION)?;
    for (id, elem_size, bytes) in netlist.sections() {
        w.section_bytes(id, elem_size, bytes)?;
    }
    Ok(w)
}

/// Encodes `netlist` into deterministic `NETLIST v1` `.rbin` bytes with no upstream header
/// entries (`WS-REQ-0120`, `WS-REQ-0121`).
pub fn encode(netlist: &FrozenNetlist<'_>) -> Result<Vec<u8>> {
    Ok(writer(netlist)?.to_bytes())
}

/// Encodes `netlist` into deterministic `NETLIST v1` `.rbin` bytes recording `upstream` in the
/// `rbin` header per `WS-REQ-0038` (`WS-REQ-0120`, `WS-REQ-0121`).
pub fn encode_with_upstream(netlist: &FrozenNetlist<'_>, upstream: &Upstream) -> Result<Vec<u8>> {
    let mut w = writer(netlist)?;
    w.upstream_tables(upstream)?;
    Ok(w.to_bytes())
}

/// Validates and opens an in-memory `.rbin` byte vector as a `NETLIST v1` [`Rbin`] container
/// (`WS-REQ-0122`).
pub fn open_bytes(bytes: Vec<u8>) -> Result<Rbin> {
    let rbin = Rbin::from_bytes(bytes, KIND, VERSIONS)?;
    rbin.verify_hash()?;
    Ok(rbin)
}

/// Decodes a [`FrozenNetlist`] zero-copy from `rbin`, borrowing all 18 sections directly from
/// `rbin`'s backing memory (`Cow::Borrowed`) and validating the netlist structure via
/// [`FrozenNetlist::validate`] (`WS-REQ-0122`, `WS-REQ-0123`).
///
/// Any header mismatch, hash mismatch, missing/extra section, element-size mismatch, or
/// [`FrozenError`] is returned as `ErrorCode::CorruptTable` (`SCHEMA_TOO_NEW` for a newer
/// format version).
pub fn decode<'a>(rbin: &'a Rbin) -> Result<FrozenNetlist<'a>> {
    let path = rbin.path();
    if rbin.kind() != KIND {
        return Err(Error::corrupt(
            path,
            Corruption::WrongKind {
                expected: KIND.to_string(),
                found: rbin.kind().to_string(),
            },
        ));
    }
    let fv = rbin.format_version();
    if fv > *VERSIONS.end() {
        return Err(Error::FormatTooNew {
            path: path.to_path_buf(),
            kind: KIND.to_string(),
            found: fv,
            min: *VERSIONS.start(),
            max: *VERSIONS.end(),
        });
    }
    if !VERSIONS.contains(&fv) {
        return Err(Error::corrupt(
            path,
            Corruption::UnsupportedVersion {
                found: fv,
                min: *VERSIONS.start(),
                max: *VERSIONS.end(),
            },
        ));
    }

    rbin.verify_hash()?;

    let frozen = FrozenNetlist {
        str_bytes: Cow::Borrowed(rbin.section::<u8>(section::STR_BYTES)?),
        str_offs: Cow::Borrowed(rbin.section::<u32>(section::STR_OFFS)?),
        module: Cow::Borrowed(rbin.section::<u32>(section::MODULE)?),
        cells: Cow::Borrowed(rbin.section::<u32>(section::CELLS)?),
        cell_pin_start: Cow::Borrowed(rbin.section::<u32>(section::CELL_PIN_START)?),
        cell_pin_name: Cow::Borrowed(rbin.section::<u32>(section::CELL_PIN_NAME)?),
        cell_pin_dir: Cow::Borrowed(rbin.section::<u8>(section::CELL_PIN_DIR)?),
        ports: Cow::Borrowed(rbin.section::<PortRec>(section::PORTS)?),
        port_bit_start: Cow::Borrowed(rbin.section::<u32>(section::PORT_BIT_START)?),
        port_bit_net: Cow::Borrowed(rbin.section::<u32>(section::PORT_BIT_NET)?),
        nets: Cow::Borrowed(rbin.section::<NetRec>(section::NETS)?),
        instances: Cow::Borrowed(rbin.section::<InstRec>(section::INSTANCES)?),
        inst_pin_start: Cow::Borrowed(rbin.section::<u32>(section::INST_PIN_START)?),
        pin_cellpin: Cow::Borrowed(rbin.section::<u32>(section::PIN_CELLPIN)?),
        pin_net: Cow::Borrowed(rbin.section::<u32>(section::PIN_NET)?),
        assigns: Cow::Borrowed(rbin.section::<AssignRec>(section::ASSIGNS)?),
        net_pin_start: Cow::Borrowed(rbin.section::<u32>(section::NET_PIN_START)?),
        net_pins: Cow::Borrowed(rbin.section::<u32>(section::NET_PINS)?),
    };

    if rbin.sections().len() != section::COUNT {
        return Err(corrupt_netlist(
            path,
            format!(
                "expected {} sections (1..={}), found {}",
                section::COUNT,
                section::COUNT,
                rbin.sections().len()
            ),
        ));
    }

    frozen
        .validate()
        .map_err(|e| corrupt_netlist(path, e.to_string()))?;
    Ok(frozen)
}

/// Validates `netlist` and stages it into `txn` as table [`TABLE_NAME`] (`netlist.rbin`) with
/// the given `upstream` map (`WS-REQ-0124`).
pub fn add_to_txn(
    txn: &mut Txn,
    netlist: &FrozenNetlist<'_>,
    upstream: Upstream,
) -> Result<Hash64> {
    netlist
        .validate()
        .map_err(|e| Error::internal(format!("staging invalid FrozenNetlist: {e}")))?;
    let w = writer(netlist)?;
    txn.add_rbin(TABLE_NAME, TABLE_PATH, &w, upstream)
}

/// Maps the committed [`TABLE_NAME`] (`netlist.rbin`) table of `rev` via [`Revision::open_rbin`]
/// (`WS-REQ-0125`).
pub fn open_rev(rev: &Revision) -> Result<Rbin> {
    rev.open_rbin(TABLE_NAME, KIND, VERSIONS)
}

/// Maps the committed [`TABLE_NAME`] (`netlist.rbin`) table of the workspace's `HEAD` revision
/// (`WS-REQ-0125`).
pub fn open_head(ws: &Workspace) -> Result<(Revision, Rbin)> {
    let rev = ws.revision(ws.head()?)?;
    let rbin = open_rev(&rev)?;
    Ok((rev, rbin))
}

/// Maps, validates, and thaws the committed [`TABLE_NAME`] table of `rev` into an editable
/// [`GateNetlist`] (`WS-REQ-0125`).
pub fn thaw_rev(rev: &Revision) -> Result<GateNetlist> {
    let rbin = open_rev(rev)?;
    let frozen = decode(&rbin)?;
    frozen
        .thaw()
        .map_err(|e| corrupt_netlist(rbin.path(), e.to_string()))
}

fn corrupt_netlist(path: &Path, reason: String) -> Error {
    Error::corrupt(path, Corruption::Json { reason })
}
