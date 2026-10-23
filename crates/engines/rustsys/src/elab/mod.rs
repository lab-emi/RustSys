// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Elaboration: parameters, types, hierarchy flattening and symbolic execution of procedures
//! into a bit-level AIG with state elements (SYNTH-REQ-01xx, -02xx, -03xx).

mod call;
mod case;
mod drive;
mod expr;
mod hier;
mod merge;
mod module;
mod pattern;
mod process;
mod resolve;
mod select;
mod sizing;
mod stmt;
pub mod types;
mod wildcards;

use std::rc::Rc;

use rustc_hash::FxHashMap;
use rusteda_formats::verilog::ast::{self, Expr, FunctionDecl, Item, TaskDecl};
use rusteda_formats::verilog::{Diagnostic, SourceMap, Span};

use crate::aig::{Aig, Lit};
use crate::bits::Bv;
pub use resolve::{Design, Port, State, StateKind};
use types::{Range, Ty};

pub type ScopeId = usize;
pub type SigId = u32;
pub type LocalId = u32;
pub type ProcId = u32;

pub(crate) type EResult<T> = Result<T, Diagnostic>;

/// A typed value: packed bits or unpacked elements (position order, left bound first).
#[derive(Debug, Clone, PartialEq)]
pub enum TVal {
    Packed(Bv, Ty),
    Unpacked(Vec<Bv>, Ty),
}

impl TVal {
    pub fn ty(&self) -> &Ty {
        match self {
            TVal::Packed(_, t) | TVal::Unpacked(_, t) => t,
        }
    }
}

/// Function definition and the scope it was declared in.
#[derive(Debug)]
pub struct FuncDef<'a> {
    pub decl: &'a FunctionDecl,
    pub scope: ScopeId,
}

#[derive(Debug)]
pub struct TaskDef<'a> {
    pub decl: &'a TaskDecl,
    pub scope: ScopeId,
}

/// Names visible in a scope.
#[derive(Debug, Clone)]
pub enum Symbol<'a> {
    Signal(SigId),
    Local(LocalId),
    /// Parameter, local parameter, enumeration constant or bound genvar.
    Const(Rc<TVal>),
    Type(Ty),
    Func(Rc<FuncDef<'a>>),
    Task(Rc<TaskDef<'a>>),
    /// Declared genvar that is not bound to a value (outside its loop).
    Genvar,
    /// Named generate block or instance.
    Scope(ScopeId),
}

#[derive(Debug, Default)]
pub struct Scope<'a> {
    pub parent: Option<ScopeId>,
    pub names: FxHashMap<String, Symbol<'a>>,
    pub imports: Vec<(String, Option<String>)>,
    /// Hierarchical path of the scope (empty for the top module and packages).
    pub path: String,
    /// Number of generate constructs seen, for `genblk<n>` names (SYNTH-REQ-0123).
    pub gen_count: u32,
    pub nettype_none: bool,
    /// Package name for package scopes.
    pub package: Option<String>,
}

/// A net or variable of the flattened design.
#[derive(Debug, Clone)]
pub struct Signal {
    pub name: String,
    pub ty: Ty,
    pub elem_ty: Ty,
    pub dims: Vec<Range>,
    pub elem_width: u32,
    pub n_elems: u32,
    /// Placeholder literal of every bit, element-major.
    pub ph: Vec<Lit>,
    /// Driver of every bit.
    pub drivers: Vec<Option<(Lit, ProcId)>>,
    pub is_net: bool,
    pub span: Span,
}

/// An automatic variable.
#[derive(Debug, Clone)]
pub struct LocalVar {
    pub name: String,
    pub ty: Ty,
}

/// A process (driver source) for diagnostics.
#[derive(Debug, Clone)]
pub struct ProcInfo {
    pub what: &'static str,
    pub span: Span,
}

/// A state bit recorded during elaboration (literals refer to the elaboration graph).
#[derive(Debug, Clone)]
pub struct StateRec {
    pub sig: SigId,
    pub elem: u32,
    pub bit: u32,
    pub latch: bool,
    pub q: Lit,
    pub d: Lit,
    pub clk: Lit,
    /// Asynchronous control (active-high condition, value).
    pub arst: Option<(Lit, bool)>,
    pub span: Span,
}

/// Key of a procedural variable element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VarRef {
    Sig(SigId),
    Local(LocalId),
}

pub type VarKey = (VarRef, u32);

/// Procedural environment (SYNTH-REQ-0210 to SYNTH-REQ-0216).
#[derive(Debug, Clone)]
pub struct Env {
    pub vals: FxHashMap<VarKey, Bv>,
    /// Per-bit condition under which a signal element was written by this procedure.
    pub assigned: FxHashMap<VarKey, Bv>,
    /// Pending nonblocking values and their per-bit written conditions.
    pub nba: FxHashMap<VarKey, (Bv, Bv)>,
    pub live: Lit,
    pub broke: Lit,
    pub continued: Lit,
    pub returned: Lit,
    /// Signals assigned with blocking assignments.
    pub blocking_sigs: FxHashMap<SigId, Span>,
    /// Signals assigned with nonblocking assignments.
    pub nba_sigs: FxHashMap<SigId, Span>,
}

impl Env {
    pub fn new() -> Env {
        Env {
            vals: FxHashMap::default(),
            assigned: FxHashMap::default(),
            nba: FxHashMap::default(),
            live: Lit::TRUE,
            broke: Lit::FALSE,
            continued: Lit::FALSE,
            returned: Lit::FALSE,
            blocking_sigs: FxHashMap::default(),
            nba_sigs: FxHashMap::default(),
        }
    }
}

impl Default for Env {
    fn default() -> Env {
        Env::new()
    }
}

/// Elaboration options.
#[derive(Debug, Clone, Default)]
pub struct ElabOptions {
    /// Keep narrow variable-index reads as mux trees when the target library
    /// implements four-way muxes more cheaply than decoded product gates.
    pub prefer_narrow_muxes: bool,
    /// Lower long arithmetic carry chains to a sparse parallel prefix.
    pub parallel_carry: bool,
    /// Reuse the sum XOR as the carry-transfer propagate signal.
    pub xor_carry: bool,
    /// Select carry blocks separately for each addition.
    pub carry_cost_weight: Option<f64>,
    /// Per wide-addition block widths in elaboration order; zero retains the default.
    pub carry_plan: Vec<usize>,
    /// Case statements marked complete take their last item as the default (SYNTH-REQ-0224).
    pub assume_full_case: bool,
    /// Case statements marked parallel merge their items without priority (SYNTH-REQ-0225).
    pub assume_parallel_case: bool,
    pub top: Option<String>,
    /// Top-level parameter overrides: name and parsed value expression.
    pub params: Vec<(String, Expr)>,
}

/// Elaboration state.
#[allow(missing_debug_implementations)]
pub struct Elaborator<'a> {
    pub sm: &'a SourceMap,
    pub aig: Aig,
    pub(crate) prefer_narrow_muxes: bool,
    pub diags: Vec<Diagnostic>,
    modules: FxHashMap<String, &'a ast::Module>,
    packages: FxHashMap<String, &'a ast::Package>,
    pub scopes: Vec<Scope<'a>>,
    unit_scope: ScopeId,
    package_scopes: FxHashMap<String, ScopeId>,
    pub signals: Vec<Signal>,
    pub placeholder_of: FxHashMap<u32, (SigId, u32)>,
    /// Large net-array reads whose implementation is chosen after all drivers are known.
    pub(crate) deferred_selects: Vec<select::DeferredSelect>,
    pub(crate) select_of: FxHashMap<u32, usize>,
    pub procs: Vec<ProcInfo>,
    pub states: Vec<StateRec>,
    var_inits: Vec<(SigId, Vec<Bv>, Span)>,
    pub locals: Vec<LocalVar>,
    pub env: Option<Env>,
    call_depth: usize,
    inst_depth: usize,
    /// Top-level ports in order: name, signal, is input.
    pub top_ports: Vec<(String, SigId, bool)>,
    pub top_name: String,
    /// Kinds of ignored constructs already reported (SYNTH-REQ-0121).
    noted: Vec<&'static str>,
    /// Nesting depth of operands where `x`/`z` literal bits are wildcards (SYNTH-REQ-0201).
    pub(crate) wildcard: u32,
    /// Complete-case assumption (SYNTH-REQ-0224).
    pub(crate) assume_full_case: bool,
    /// Parallel-case assumption (SYNTH-REQ-0225).
    pub(crate) assume_parallel_case: bool,
    /// Literals already warned about for `z` bits (SYNTH-REQ-0217).
    z_warned: rustc_hash::FxHashSet<Span>,
    /// Exit accumulators of enclosing loops and subroutines.
    pub(crate) exits: Vec<stmt::Exits>,
    /// Return variable of each active function call.
    pub(crate) ret_stack: Vec<Option<LocalId>>,
    pub(crate) ret_scopes: Vec<ScopeId>,
    /// Names of module-level data declarations (non-ANSI port refinement).
    pub(crate) port_data_names: rustc_hash::FxHashSet<String>,
    /// Graph nodes of primary input bits.
    pub(crate) top_input_nodes: rustc_hash::FxHashSet<u32>,
    /// Terms of the sums built so far, by result bits (SYNTH-REQ-0407).
    pub(crate) sums: FxHashMap<Bv, Rc<crate::bits::Sum>>,
}

/// Maximum module instance nesting.
const MAX_INSTANCE_DEPTH: usize = 256;
/// Maximum function call nesting (SYNTH-REQ-0132).
const MAX_CALL_DEPTH: usize = 256;

impl<'a> Elaborator<'a> {
    pub fn new(sm: &'a SourceMap, units: &'a [ast::SourceUnit]) -> Elaborator<'a> {
        let mut e = Elaborator {
            sm,
            aig: Aig::new(),
            prefer_narrow_muxes: false,
            diags: Vec::new(),
            modules: FxHashMap::default(),
            packages: FxHashMap::default(),
            scopes: vec![Scope::default()],
            unit_scope: 0,
            package_scopes: FxHashMap::default(),
            signals: Vec::new(),
            placeholder_of: FxHashMap::default(),
            deferred_selects: Vec::new(),
            select_of: FxHashMap::default(),
            procs: Vec::new(),
            states: Vec::new(),
            var_inits: Vec::new(),
            locals: Vec::new(),
            env: None,
            call_depth: 0,
            inst_depth: 0,
            top_ports: Vec::new(),
            top_name: String::new(),
            noted: Vec::new(),
            wildcard: 0,
            assume_full_case: false,
            assume_parallel_case: false,
            z_warned: Default::default(),
            sums: Default::default(),
            exits: Vec::new(),
            ret_stack: Vec::new(),
            ret_scopes: Vec::new(),
            port_data_names: rustc_hash::FxHashSet::default(),
            top_input_nodes: rustc_hash::FxHashSet::default(),
        };
        for u in units {
            for it in &u.items {
                match it {
                    Item::Module(m) => {
                        if e.modules.insert(m.name.name.clone(), m).is_some() {
                            e.warn("S0101", m.name.span, format!("module '{}' is defined more than once; the last definition is used", m.name.name));
                        }
                    }
                    Item::Package(p) => {
                        e.packages.insert(p.name.name.clone(), p);
                    }
                    _ => {}
                }
            }
        }
        e
    }

    pub(crate) fn err(&self, code: &'static str, span: Span, msg: impl Into<String>) -> Diagnostic {
        Diagnostic::error(code, span, msg)
    }

    pub(crate) fn warn(&mut self, code: &'static str, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic::warning(code, span, msg));
    }

    /// Warns once per literal that its `z` bits are read as 0 outside wildcard operands
    /// (SYNTH-REQ-0217).
    pub(crate) fn warn_z(&mut self, span: Span) {
        if self.wildcard == 0 && self.z_warned.insert(span) {
            self.warn(
                "S0125",
                span,
                "`z` bits are read as 0; no tri-state logic is produced (SYNTH-REQ-0217)",
            );
        }
    }

    pub(crate) fn note_once(&mut self, what: &'static str, span: Span) {
        if !self.noted.contains(&what) {
            self.noted.push(what);
            self.diags.push(Diagnostic::new(
                rusteda_formats::verilog::Severity::Note,
                "S0120",
                span,
                format!("{what} ignored for synthesis"),
            ));
        }
    }

    pub(crate) fn new_scope(&mut self, parent: ScopeId, path: String) -> ScopeId {
        let nettype_none = self.scopes[parent].nettype_none;
        self.scopes.push(Scope {
            parent: Some(parent),
            path,
            nettype_none,
            ..Scope::default()
        });
        self.scopes.len() - 1
    }

    pub(crate) fn declare(
        &mut self,
        scope: ScopeId,
        name: &str,
        sym: Symbol<'a>,
        span: Span,
    ) -> EResult<()> {
        if let Some(old) = self.scopes[scope].names.get(name) {
            // Genvar declarations may be followed by their loop binding.
            if !matches!(old, Symbol::Genvar) {
                return Err(self.err(
                    "S0102",
                    span,
                    format!("'{name}' is already declared in this scope"),
                ));
            }
        }
        self.scopes[scope].names.insert(name.to_string(), sym);
        Ok(())
    }

    /// Looks up a name from a scope outwards, including package imports.
    pub(crate) fn lookup(&self, scope: ScopeId, name: &str) -> Option<Symbol<'a>> {
        let mut s = Some(scope);
        while let Some(id) = s {
            let sc = &self.scopes[id];
            if let Some(sym) = sc.names.get(name) {
                return Some(sym.clone());
            }
            for (pkg, item) in &sc.imports {
                if item.as_deref().is_none_or(|i| i == name) {
                    if let Some(ps) = self.package_scopes.get(pkg) {
                        if let Some(sym) = self.scopes[*ps].names.get(name) {
                            return Some(sym.clone());
                        }
                    }
                }
            }
            s = sc.parent;
        }
        None
    }

    /// Looks up `pkg::name`.
    pub(crate) fn lookup_path(&self, scope: ScopeId, path: &ast::Path) -> EResult<Symbol<'a>> {
        match &path.scope {
            Some(pkg) => {
                let ps = self.package_scopes.get(&pkg.name).ok_or_else(|| {
                    self.err("S0103", pkg.span, format!("unknown package '{}'", pkg.name))
                })?;
                self.scopes[*ps]
                    .names
                    .get(&path.name.name)
                    .cloned()
                    .ok_or_else(|| {
                        self.err(
                            "S0104",
                            path.name.span,
                            format!(
                                "'{}' is not declared in package '{}'",
                                path.name.name, pkg.name
                            ),
                        )
                    })
            }
            None => self.lookup(scope, &path.name.name).ok_or_else(|| {
                self.err(
                    "S0105",
                    path.name.span,
                    format!("'{}' is not declared", path.name.name),
                )
            }),
        }
    }

    /// Hierarchical name of a declaration in a scope.
    pub(crate) fn hier_name(&self, scope: ScopeId, name: &str) -> String {
        let p = &self.scopes[scope].path;
        if p.is_empty() {
            name.to_string()
        } else {
            format!("{p}.{name}")
        }
    }

    /// Creates a signal with placeholders.
    pub(crate) fn new_signal(
        &mut self,
        name: String,
        ty: Ty,
        is_net: bool,
        span: Span,
    ) -> EResult<SigId> {
        let (dims, elem) = ty.unpacked_dims();
        let elem_ty = elem.clone();
        let elem_width = elem_ty.width();
        let n_elems = ty.element_count();
        if elem_width == 0 {
            return Err(self.err("S0106", span, format!("signal '{name}' has zero width")));
        }
        let total = u64::from(elem_width) * u64::from(n_elems);
        if total > 1 << 24 {
            return Err(self.err(
                "S0107",
                span,
                format!("signal '{name}' is too large ({total} bits)"),
            ));
        }
        let id = self.signals.len() as SigId;
        let mut ph = Vec::with_capacity(total as usize);
        for b in 0..total as u32 {
            let l = self.aig.input();
            self.placeholder_of.insert(l.node(), (id, b));
            ph.push(l);
        }
        self.signals.push(Signal {
            name,
            ty,
            elem_ty,
            dims,
            elem_width,
            n_elems,
            ph,
            drivers: vec![None; total as usize],
            is_net,
            span,
        });
        Ok(id)
    }

    pub(crate) fn new_local(&mut self, name: &str, ty: Ty) -> LocalId {
        self.locals.push(LocalVar {
            name: name.to_string(),
            ty,
        });
        (self.locals.len() - 1) as LocalId
    }

    pub(crate) fn new_proc(&mut self, what: &'static str, span: Span) -> ProcId {
        self.procs.push(ProcInfo { what, span });
        (self.procs.len() - 1) as ProcId
    }

    /// Records a driver for one signal bit (SYNTH-REQ-0210).
    pub(crate) fn drive(
        &mut self,
        sig: SigId,
        bit: u32,
        lit: Lit,
        proc: ProcId,
        span: Span,
    ) -> EResult<()> {
        let s = &mut self.signals[sig as usize];
        match s.drivers[bit as usize] {
            Some((_, p)) if p != proc => {
                let other = self.procs[p as usize].span;
                let name = self.signals[sig as usize].name.clone();
                let loc = self.sm.describe(other);
                Err(self.err(
                    "S0201",
                    span,
                    format!(
                        "bit {bit} of '{name}' has more than one driver (other driver at {loc})"
                    ),
                ))
            }
            _ => {
                s.drivers[bit as usize] = Some((lit, proc));
                Ok(())
            }
        }
    }

    /// Current value of a variable element in the procedural environment.
    pub(crate) fn read_var(&self, key: VarKey) -> Bv {
        if let Some(env) = &self.env {
            if let Some(v) = env.vals.get(&key) {
                return v.clone();
            }
        }
        self.initial_value(key)
    }

    /// Value of a variable element before the procedure started.
    pub(crate) fn initial_value(&self, key: VarKey) -> Bv {
        match key.0 {
            VarRef::Sig(s) => {
                let sg = &self.signals[s as usize];
                let w = sg.elem_width as usize;
                sg.ph[key.1 as usize * w..(key.1 as usize + 1) * w].to_vec()
            }
            VarRef::Local(l) => {
                let (_, elem) = self.locals[l as usize].ty.unpacked_dims();
                vec![Lit::FALSE; elem.width() as usize]
            }
        }
    }

    /// Type of a variable.
    pub(crate) fn var_ty(&self, v: VarRef) -> &Ty {
        match v {
            VarRef::Sig(s) => &self.signals[s as usize].ty,
            VarRef::Local(l) => &self.locals[l as usize].ty,
        }
    }

    /// State name of a signal bit (SYNTH-REQ-0603).
    pub fn state_name(&self, sig: SigId, elem: u32, bit: u32) -> String {
        let s = &self.signals[sig as usize];
        let mut name = s.name.clone();
        // Element position to indices, outer dimension first.
        let mut rem = elem;
        let mut idx = Vec::with_capacity(s.dims.len());
        for (k, d) in s.dims.iter().enumerate() {
            let inner: u32 = s.dims[k + 1..]
                .iter()
                .map(Range::len)
                .product::<u32>()
                .max(1);
            let p = rem / inner;
            rem %= inner;
            idx.push(d.index_at_position(p));
        }
        for i in idx {
            name.push_str(&format!("[{i}]"));
        }
        if s.elem_width > 1 {
            name.push_str(&format!("[{bit}]"));
        }
        name
    }
}

/// Runs elaboration of the given top module and resolves the design.
pub fn elaborate<'a>(
    sm: &'a SourceMap,
    units: &'a [ast::SourceUnit],
    opts: &'a ElabOptions,
) -> (Option<Design>, Vec<Diagnostic>) {
    elaborate_impl(sm, units, opts, false)
}

/// Elaborate with bounded multiplier observations for an external proof driver.
/// Production synthesis uses `elaborate` and does not allocate these observations.
pub fn elaborate_with_multiplier_trace<'a>(
    sm: &'a SourceMap,
    units: &'a [ast::SourceUnit],
    opts: &'a ElabOptions,
) -> (Option<Design>, Vec<Diagnostic>) {
    elaborate_impl(sm, units, opts, true)
}

fn elaborate_impl<'a>(
    sm: &'a SourceMap,
    units: &'a [ast::SourceUnit],
    opts: &'a ElabOptions,
    trace: bool,
) -> (Option<Design>, Vec<Diagnostic>) {
    let mut e = Elaborator::new(sm, units);
    e.prefer_narrow_muxes = opts.prefer_narrow_muxes;
    if trace {
        e.aig.mul_sites = Some(Vec::new());
    }
    e.aig.parallel_carry = opts.parallel_carry;
    e.aig.xor_carry = opts.xor_carry;
    e.aig.carry_cost_weight = opts.carry_cost_weight;
    e.aig.carry_plan = opts.carry_plan.clone();
    e.assume_full_case = opts.assume_full_case;
    e.assume_parallel_case = opts.assume_parallel_case;
    let r = e.run(units, opts);
    match r {
        Ok(()) if !e.diags.iter().any(Diagnostic::is_error) => match e.resolve() {
            Ok(d) => {
                let diags = std::mem::take(&mut e.diags);
                (Some(d), diags)
            }
            Err(d) => {
                e.diags.push(d);
                (None, std::mem::take(&mut e.diags))
            }
        },
        Ok(()) => (None, std::mem::take(&mut e.diags)),
        Err(d) => {
            e.diags.push(d);
            (None, std::mem::take(&mut e.diags))
        }
    }
}
