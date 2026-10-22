// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The Liberty library model, specified in `docs/specs/core/` (section "Liberty library
//! model"): units, cells, pins, timing arcs, lookup tables, Boolean functions and the enumerated
//! attribute values. The reader that builds it from Liberty text is `rusteda_formats::liberty`
//! (`docs/specs/liberty/`).

mod func;
mod kinds;
mod model;
mod table;
mod units;

pub use func::{Func, var_mask};
pub use kinds::{
    AntennaDiodeType, LevelShifterType, PgType, SwitchCellType, TableVariable, TimingSense,
    TimingType, TreeType, WireLoadMode,
};
pub use model::{
    Cell, Direction, InternalPower, Library, OperatingConditions, PgPin, Pin, SeqKind, Sequential,
    Statetable, Template, TimingArc, WireLoad, WireLoadAreaRule, WireLoadSelection, WireLoadTable,
};
pub use table::Table;
pub use units::{Unit, Units};
