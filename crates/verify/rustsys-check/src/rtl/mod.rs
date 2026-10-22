// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! The checker's own model of the RTL (CHECK-REQ-0010, CHECK-REQ-0011).
//!
//! Every net and variable bit is a circuit variable. Continuous assignments, gates, port
//! connections and procedures define these variables as aliases of the functions that drive
//! them. Procedures are interpreted statement by statement with path conditions: every write is
//! guarded by the condition under which it executes.

mod always;
mod assign;
mod call;
mod elab;
mod eval;
mod hier;
mod labels;
mod proc;
mod size;
pub mod ty;
mod word;

use std::collections::HashMap;
use std::rc::Rc;

use rusteda_formats::verilog::ast::{FunctionDecl, TaskDecl};
use rusteda_formats::verilog::{SourceMap, Span};

use crate::circ::{B, Circuit};
use ty::Ty;

pub(crate) use always::PROOF_BUDGET;
pub(crate) use elab::top_name;

/// A value: packed bits or unpacked elements (left bound first).
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    P(Vec<B>),
    U(Vec<Vec<B>>),
}

/// A name in a scope.
#[derive(Debug, Clone)]
pub(crate) enum Name<'a> {
    Sig(usize),
    Local(usize),
    Value(Rc<(Val, Ty)>),
    Type(Ty),
    Func(&'a FunctionDecl, usize),
    Task(&'a TaskDecl, usize),
    Genvar,
    Instance,
}

#[derive(Debug, Default)]
pub(crate) struct ScopeRec<'a> {
    pub up: Option<usize>,
    pub names: HashMap<String, Name<'a>>,
    pub imports: Vec<(String, Option<String>)>,
    pub path: String,
    pub gen_n: u32,
    pub nettype_none: bool,
}

/// A net or variable.
#[derive(Debug, Clone)]
pub(crate) struct SigRec {
    pub name: String,
    pub ty: Ty,
    /// Circuit variable of every bit, element-major.
    pub vars: Vec<u32>,
    /// Driver (process number) of every bit.
    pub driver: Vec<Option<usize>>,
    pub span: Span,
}

/// An RTL state element (CHECK-REQ-0011).
#[derive(Debug, Clone)]
pub struct RtlState {
    pub name: String,
    pub latch: bool,
    /// Current value (a free variable).
    pub q: u32,
    /// Next value (flip-flops) or data (latches).
    pub next: B,
    /// Rising-edge clock (flip-flops) or active-high enable (latches).
    pub clk: B,
    pub arst: Option<(B, bool)>,
}

/// The RTL model.
#[derive(Debug, Clone)]
pub struct RtlModel {
    pub top: String,
    /// Input ports: name, free variables (least significant first), scalar.
    pub inputs: Vec<(String, Vec<u32>, bool)>,
    /// Output ports: name, bits, scalar.
    pub outputs: Vec<(String, Vec<B>, bool)>,
    /// Port order: (is input, index into inputs/outputs).
    pub port_order: Vec<(bool, usize)>,
    pub states: Vec<RtlState>,
    /// Optional internal observations; they supplement the original interface.
    pub observations: Vec<(ObservationProbe, Vec<B>)>,
    pub warnings: Vec<String>,
}

/// A bounded packed-signal slice for external compositional verification.
/// Offsets are least-significant-bit ordinals, independent of declared bounds.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ObservationProbe {
    pub name: String,
    pub offset: usize,
    pub width: usize,
}

/// Options shared with the synthesis command (CHECK-REQ-0001).
#[derive(Debug, Clone, Default)]
pub struct RtlOptions {
    pub top: Option<String>,
    pub params: Vec<(String, rusteda_formats::verilog::ast::Expr)>,
    /// Complete-case assumption (CHECK-REQ-0012).
    pub assume_full_case: bool,
    /// Parallel-case assumption (CHECK-REQ-0013).
    pub assume_parallel_case: bool,
    /// Additional observation outputs; never assumptions or trusted cut points.
    pub observations: Vec<ObservationProbe>,
}

pub(crate) type R<T> = Result<T, String>;

/// Why the RTL model was not built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    /// The RTL is erroneous or outside the supported subset.
    Input(String),
    /// A proof that the model needs ran out of its conflict budget (CHECK-REQ-0002).
    Undecided(String),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::Input(m) | BuildError::Undecided(m) => f.write_str(m),
        }
    }
}

/// Model builder.
pub(crate) struct Builder<'a> {
    pub sm: &'a SourceMap,
    pub c: &'a mut Circuit,
    pub modules: HashMap<String, &'a rusteda_formats::verilog::ast::Module>,
    pub packages: HashMap<String, usize>,
    pub scopes: Vec<ScopeRec<'a>>,
    pub sigs: Vec<SigRec>,
    pub locals: Vec<Ty>,
    pub nprocs: usize,
    pub states: Vec<RtlState>,
    pub warnings: Vec<String>,
    pub inits: Vec<(usize, Val)>,
    /// Procedural state while interpreting a procedure.
    pub ps: Option<proc::PState>,
    /// Forced signal variables (asynchronous-control analysis).
    pub forced: HashMap<u32, bool>,
    pub depth: usize,
    /// Frame index of each active subroutine call.
    pub ret_frames: Vec<usize>,
    /// Return variable (and its scope) of each active call.
    pub ret_vars: Vec<Option<(usize, usize)>>,
    /// Path condition of statement-level calls.
    pub call_guard: Vec<B>,
    /// Case statements marked complete use their last item as the default (CHECK-REQ-0012).
    pub assume_full_case: bool,
    /// Case statements marked parallel OR their items' results (CHECK-REQ-0013).
    pub assume_parallel_case: bool,
    /// Conflict budget of each proof that the model needs.
    pub proof_budget: u64,
    /// The message of a proof that ran out of its budget, when that stopped the build.
    pub undecided: Option<String>,
}

impl<'a> Builder<'a> {
    pub fn loc(&self, sp: Span) -> String {
        self.sm.describe(sp)
    }

    pub fn fail<T>(&self, sp: Span, msg: impl Into<String>) -> R<T> {
        Err(format!("{}: {}", self.loc(sp), msg.into()))
    }

    /// Stops because a proof that the model needs ran out of its conflict budget: `build`
    /// reports it as undecided, not as an error of the RTL (CHECK-REQ-0002).
    pub fn undecided<T>(&mut self, sp: Span, msg: impl Into<String>) -> R<T> {
        let m = format!("{}: {}", self.loc(sp), msg.into());
        self.undecided = Some(m.clone());
        Err(m)
    }

    pub fn scope(&mut self, up: usize, path: String) -> usize {
        let nn = self.scopes[up].nettype_none;
        self.scopes.push(ScopeRec {
            up: Some(up),
            path,
            nettype_none: nn,
            ..ScopeRec::default()
        });
        self.scopes.len() - 1
    }

    pub fn put(&mut self, sc: usize, n: &str, v: Name<'a>, sp: Span) -> R<()> {
        if let Some(old) = self.scopes[sc].names.get(n) {
            if !matches!(old, Name::Genvar) {
                return self.fail(sp, format!("'{n}' declared twice"));
            }
        }
        self.scopes[sc].names.insert(n.to_string(), v);
        Ok(())
    }

    pub fn find(&self, sc: usize, n: &str) -> Option<Name<'a>> {
        let mut s = Some(sc);
        while let Some(i) = s {
            if let Some(x) = self.scopes[i].names.get(n) {
                return Some(x.clone());
            }
            for (p, item) in &self.scopes[i].imports {
                if item.as_deref().is_none_or(|x| x == n) {
                    if let Some(ps) = self.packages.get(p) {
                        if let Some(x) = self.scopes[*ps].names.get(n) {
                            return Some(x.clone());
                        }
                    }
                }
            }
            s = self.scopes[i].up;
        }
        None
    }

    pub fn find_path(&self, sc: usize, p: &rusteda_formats::verilog::ast::Path) -> R<Name<'a>> {
        match &p.scope {
            Some(pkg) => {
                let Some(ps) = self.packages.get(&pkg.name) else {
                    return self.fail(pkg.span, format!("unknown package {}", pkg.name));
                };
                match self.scopes[*ps].names.get(&p.name.name) {
                    Some(x) => Ok(x.clone()),
                    None => self.fail(
                        p.name.span,
                        format!("{} not in package {}", p.name.name, pkg.name),
                    ),
                }
            }
            None => match self.find(sc, &p.name.name) {
                Some(x) => Ok(x),
                None => self.fail(p.name.span, format!("undeclared name {}", p.name.name)),
            },
        }
    }

    pub fn full(&self, sc: usize, n: &str) -> String {
        let p = &self.scopes[sc].path;
        if p.is_empty() {
            n.to_string()
        } else {
            format!("{p}.{n}")
        }
    }

    pub fn new_sig(&mut self, name: String, ty: Ty, sp: Span) -> R<usize> {
        let n = ty.width() * ty.count();
        if n == 0 || n > 1 << 24 {
            return self.fail(sp, format!("bad size of {name}"));
        }
        let vars: Vec<u32> = (0..n).map(|_| self.c.free()).collect();
        self.sigs.push(SigRec {
            name,
            ty,
            vars,
            driver: vec![None; n as usize],
            span: sp,
        });
        Ok(self.sigs.len() - 1)
    }

    /// Defines one signal bit (SYNTH-REQ-0210).
    pub fn set(&mut self, s: usize, bit: usize, b: B, pr: usize, sp: Span) -> R<()> {
        match self.sigs[s].driver[bit] {
            Some(p) if p != pr => self.fail(
                sp,
                format!("bit {bit} of {} has two drivers", self.sigs[s].name),
            ),
            _ => {
                self.sigs[s].driver[bit] = Some(pr);
                let v = self.sigs[s].vars[bit];
                if !self.c.define(v, b) {
                    return self.fail(
                        sp,
                        format!("bit {bit} of {} defined twice", self.sigs[s].name),
                    );
                }
                Ok(())
            }
        }
    }

    pub fn proc_id(&mut self) -> usize {
        self.nprocs += 1;
        self.nprocs
    }

    /// State name of a signal bit (SYNTH-REQ-0603), written independently of the engine.
    pub fn bit_name(&self, s: usize, flat: usize) -> String {
        let sg = &self.sigs[s];
        let w = sg.ty.width() as usize;
        let (elem, bit) = (flat / w, flat % w);
        let mut out = sg.name.clone();
        // Element index to per-dimension indices, outer dimension first.
        let sizes: Vec<u64> = sg
            .ty
            .unpacked
            .iter()
            .map(|(l, r)| (l - r).unsigned_abs() + 1)
            .collect();
        let mut rem = elem as u64;
        for (k, (l, r)) in sg.ty.unpacked.iter().enumerate() {
            let inner: u64 = sizes[k + 1..].iter().product();
            let p = rem / inner.max(1);
            rem %= inner.max(1);
            out.push_str(&format!("[{}]", ty::index_at(*l, *r, p)));
        }
        if w > 1 {
            out.push_str(&format!("[{bit}]"));
        }
        out
    }
}

/// Builds the RTL model with the default conflict budget of the proofs that the model needs.
pub fn build(
    sm: &SourceMap,
    units: &[rusteda_formats::verilog::ast::SourceUnit],
    c: &mut Circuit,
    opts: &RtlOptions,
) -> Result<RtlModel, BuildError> {
    build_with(sm, units, c, opts, PROOF_BUDGET)
}

/// `build` with a conflict budget for the proofs that the model needs (CHECK-REQ-0002).
pub(crate) fn build_with(
    sm: &SourceMap,
    units: &[rusteda_formats::verilog::ast::SourceUnit],
    c: &mut Circuit,
    opts: &RtlOptions,
    proof_budget: u64,
) -> Result<RtlModel, BuildError> {
    let mut b = Builder {
        sm,
        c,
        modules: HashMap::new(),
        packages: HashMap::new(),
        scopes: vec![ScopeRec::default()],
        sigs: Vec::new(),
        locals: Vec::new(),
        nprocs: 0,
        states: Vec::new(),
        warnings: Vec::new(),
        inits: Vec::new(),
        ps: None,
        forced: HashMap::new(),
        depth: 0,
        ret_frames: Vec::new(),
        ret_vars: Vec::new(),
        call_guard: Vec::new(),
        assume_full_case: opts.assume_full_case,
        assume_parallel_case: opts.assume_parallel_case,
        proof_budget,
        undecided: None,
    };
    b.run(units, opts).map_err(|m| {
        if b.undecided.as_deref() == Some(m.as_str()) {
            BuildError::Undecided(m)
        } else {
            BuildError::Input(m)
        }
    })
}
