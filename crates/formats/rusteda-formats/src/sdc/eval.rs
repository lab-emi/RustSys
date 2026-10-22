// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Bounded deterministic Tcl evaluator for SDC import (SDC-REQ-0001, SDC-REQ-0007 to
//! SDC-REQ-0015).

use std::collections::BTreeMap;

use rusteda_core::diag::{Diag, Severity};

use super::cmd::Cmd;
use super::expr::eval_expr_str;
use super::obj::Obj;
use super::schema::{ActiveUnits, find_query_schema, parse_query_cmd, parse_top_cmd};
use super::{Env, EnvEntry, Limits, Sdc, Source, SourceRecord, Units};
use builtins::{
    ParsedCommand, Segment, WordAst, WordKind, normalize_var_name, parse_env_var, parse_list_index,
    parse_script_commands,
};

/// Runtime value in the bounded Tcl interpreter.
#[derive(Debug, Clone)]
pub enum TclVal {
    Str(String),
    List(Vec<TclVal>),
    Obj(Obj),
    SearchIndex { haystack: Obj, needle: Obj },
}

impl TclVal {
    /// Returns the string slice if this value is a scalar string starting with `'-'`.
    pub fn as_option_token(&self) -> Option<&str> {
        match self {
            TclVal::Str(s) if s.starts_with('-') => Some(s.as_str()),
            _ => None,
        }
    }

    /// Converts a scalar or flat list value to a single string; returns `None` for object queries.
    pub fn as_scalar_str(&self) -> Option<String> {
        match self {
            TclVal::Str(s) => Some(s.clone()),
            TclVal::List(items) => {
                let mut parts = Vec::with_capacity(items.len());
                for item in items {
                    parts.push(item.as_scalar_str()?);
                }
                Some(parts.join(" "))
            }
            TclVal::Obj(_) | TclVal::SearchIndex { .. } => None,
        }
    }

    /// Converts a string or list value into a list of strings; returns `None` for object queries.
    pub fn to_string_list(&self) -> Option<Vec<String>> {
        match self {
            TclVal::Str(s) => {
                let list = split_tcl_list(s);
                if list.is_empty() && !s.is_empty() && !s.chars().all(char::is_whitespace) {
                    Some(vec![s.clone()])
                } else {
                    Some(list)
                }
            }
            TclVal::List(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        TclVal::Str(s) => out.push(s.clone()),
                        TclVal::List(_) => out.extend(item.to_string_list()?),
                        TclVal::Obj(_) | TclVal::SearchIndex { .. } => return None,
                    }
                }
                Some(out)
            }
            TclVal::Obj(_) | TclVal::SearchIndex { .. } => None,
        }
    }

    /// Converts this value into a symbolic [`Obj`] query or `Obj::Names` list.
    pub fn to_obj(&self) -> Option<Obj> {
        match self {
            TclVal::Obj(o) => Some(o.clone()),
            TclVal::List(items) => {
                if items.is_empty() {
                    return None;
                }
                let has_obj = items.iter().any(Self::contains_obj);
                if has_obj {
                    let mut objs = Vec::with_capacity(items.len());
                    for item in items {
                        objs.push(item.to_obj()?);
                    }
                    Some(Obj::union(objs))
                } else {
                    let mut names = Vec::with_capacity(items.len());
                    for item in items {
                        names.extend(item.to_string_list()?);
                    }
                    if names.is_empty() {
                        None
                    } else {
                        Some(Obj::Names(names))
                    }
                }
            }
            TclVal::Str(s) => {
                let names = split_tcl_list(s);
                if names.is_empty() {
                    None
                } else {
                    Some(Obj::Names(names))
                }
            }
            TclVal::SearchIndex { .. } => None,
        }
    }

    fn contains_obj(&self) -> bool {
        match self {
            TclVal::Obj(_) => true,
            TclVal::List(items) => items.iter().any(Self::contains_obj),
            TclVal::Str(_) | TclVal::SearchIndex { .. } => false,
        }
    }
}

/// Splits a Tcl list string into its element strings.
pub fn split_tcl_list(input: &str) -> Vec<String> {
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        match bytes[i] {
            b'{' => {
                i += 1;
                let start = i;
                let mut depth = 1usize;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'{' {
                        depth += 1;
                    } else if bytes[i] == b'}' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    i += 1;
                }
                out.push(input[start..i].to_string());
                if i < bytes.len() && bytes[i] == b'}' {
                    i += 1;
                }
            }
            b'"' => {
                i += 1;
                let mut s = String::new();
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 1;
                        s.push(unescape_char(bytes[i]));
                        i += 1;
                    } else {
                        let ch = input[i..].chars().next().unwrap();
                        s.push(ch);
                        i += ch.len_utf8();
                    }
                }
                if i < bytes.len() && bytes[i] == b'"' {
                    i += 1;
                }
                out.push(s);
            }
            _ => {
                let mut s = String::new();
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 1;
                        s.push(unescape_char(bytes[i]));
                        i += 1;
                    } else {
                        let ch = input[i..].chars().next().unwrap();
                        s.push(ch);
                        i += ch.len_utf8();
                    }
                }
                out.push(s);
            }
        }
    }
    out
}

pub(super) fn unescape_char(b: u8) -> char {
    match b {
        b'n' => '\n',
        b'r' => '\r',
        b't' => '\t',
        other => other as char,
    }
}

/// Evaluation error carrying a `D`-code, message, and 1-based `(file, line, col)`.
#[derive(Debug, Clone)]
pub struct EvalError {
    pub code: &'static str,
    pub message: String,
    pub file: String,
    pub line: u32,
    pub col: u32,
}

impl EvalError {
    pub fn new(code: &'static str, message: impl Into<String>, loc: (&str, u32, u32)) -> Self {
        Self {
            code,
            message: message.into(),
            file: loc.0.to_string(),
            line: loc.1,
            col: loc.2,
        }
    }

    pub fn into_diag(self) -> Diag {
        Diag::new(Severity::Error, self.code, self.message).at(self.file, self.line, self.col)
    }
}

/// Bounded Tcl interpreter state (SDC-REQ-0007 to SDC-REQ-0015).
pub struct Interpreter<'a> {
    limits: &'a Limits,
    steps: u64,
    depth: u32,
    vars: BTreeMap<String, TclVal>,
    env_map: BTreeMap<String, String>,
    env_entries: Vec<EnvEntry>,
    active_units: ActiveUnits,
    pub sdc_units: Units,
    pub current_design: Option<String>,
    pub sources: Vec<SourceRecord>,
    pub cmds: Vec<Cmd>,
    pub diags: Vec<Diag>,
    current_source_idx: u16,
}

impl<'a> Interpreter<'a> {
    /// Creates a new interpreter initialized with `env` and `limits`.
    pub fn new(env: &Env, limits: &'a Limits) -> Self {
        let mut env_map = BTreeMap::new();
        let mut env_entries = Vec::new();
        for (k, v) in &env.vars {
            env_map.insert(k.clone(), v.clone());
            if let Some(existing) = env_entries
                .iter_mut()
                .find(|e: &&mut EnvEntry| e.name == *k)
            {
                existing.value = v.clone();
            } else {
                env_entries.push(EnvEntry {
                    name: k.clone(),
                    value: v.clone(),
                    written: false,
                });
            }
        }
        Self {
            limits,
            steps: 0,
            depth: 0,
            vars: BTreeMap::new(),
            env_map,
            env_entries,
            active_units: ActiveUnits::default(),
            sdc_units: Units::default(),
            current_design: None,
            sources: Vec::new(),
            cmds: Vec::new(),
            diags: Vec::new(),
            current_source_idx: 0,
        }
    }

    /// Consumes the interpreter and returns the assembled `(Sdc, Vec<Diag>)`.
    pub fn finish(self) -> (Sdc, Vec<Diag>) {
        let diags_copy = self.diags.clone();
        let sdc = Sdc {
            units: self.sdc_units,
            current_design: self.current_design,
            sources: self.sources,
            env: self.env_entries,
            cmds: self.cmds,
            diags: self.diags,
        };
        (sdc, diags_copy)
    }

    /// Increments the evaluation step counter and checks [`Limits::max_steps`] (SDC-REQ-0015).
    pub fn step(&mut self, loc: (&str, u32, u32)) -> Result<(), EvalError> {
        self.steps += 1;
        if self.steps > self.limits.max_steps {
            return Err(EvalError::new(
                "D0008",
                format!(
                    "evaluation step budget exceeded (limit: {} steps)",
                    self.limits.max_steps
                ),
                loc,
            ));
        }
        Ok(())
    }

    /// Evaluates a top-level [`Source`] file.
    pub fn eval_top_source(&mut self, src: &Source) {
        let idx = self.sources.len() as u16;
        self.sources.push(SourceRecord {
            path: src.name.clone(),
            flags: 0,
            xxh3: src.xxh3,
        });
        let prev_idx = self.current_source_idx;
        self.current_source_idx = idx;
        if let Err(e) = self.eval_script(&src.text, &src.name, 1, 1, true) {
            self.diags.push(e.into_diag());
        }
        self.current_source_idx = prev_idx;
    }

    /// Evaluates a nested `[cmd ...]` script and returns its result value.
    pub fn eval_nested_script(
        &mut self,
        script: &str,
        loc: (&str, u32, u32),
    ) -> Result<TclVal, EvalError> {
        self.depth += 1;
        if self.depth > self.limits.max_depth {
            self.depth -= 1;
            return Err(EvalError::new(
                "D0008",
                format!(
                    "evaluation recursion depth exceeded (limit: {})",
                    self.limits.max_depth
                ),
                loc,
            ));
        }
        let res = self.eval_script(script, loc.0, loc.1, loc.2, false);
        self.depth -= 1;
        res
    }

    /// Looks up a variable or `::env(X)` binding (SDC-REQ-0008, SDC-REQ-0009).
    pub fn lookup_var(
        &mut self,
        raw_name: &str,
        loc: (&str, u32, u32),
    ) -> Result<TclVal, EvalError> {
        let norm = normalize_var_name(raw_name);
        if let Some(env_key) = parse_env_var(norm) {
            if let Some(val) = self.env_map.get(env_key) {
                return Ok(TclVal::Str(val.clone()));
            }
            return Err(EvalError::new(
                "D0007",
                format!("undefined environment variable '::env({env_key})'"),
                loc,
            ));
        }
        if let Some(v) = self.vars.get(norm) {
            return Ok(v.clone());
        }
        Err(EvalError::new(
            "D0007",
            format!("undefined variable '{raw_name}'"),
            loc,
        ))
    }

    fn set_var(
        &mut self,
        raw_name: &str,
        val: TclVal,
        loc: (&str, u32, u32),
    ) -> Result<TclVal, EvalError> {
        let norm = normalize_var_name(raw_name);
        if let Some(env_key) = parse_env_var(norm) {
            let s = val.as_scalar_str().ok_or_else(|| {
                EvalError::new(
                    "D0005",
                    format!("cannot assign object collection to '::env({env_key})'"),
                    loc,
                )
            })?;
            self.env_map.insert(env_key.to_string(), s.clone());
            if let Some(entry) = self.env_entries.iter_mut().find(|e| e.name == env_key) {
                entry.value = s.clone();
                entry.written = true;
            } else {
                self.env_entries.push(EnvEntry {
                    name: env_key.to_string(),
                    value: s.clone(),
                    written: true,
                });
            }
            return Ok(TclVal::Str(s));
        }
        self.vars.insert(norm.to_string(), val.clone());
        Ok(val)
    }

    fn var_exists(&self, raw_name: &str) -> bool {
        let norm = normalize_var_name(raw_name);
        if let Some(env_key) = parse_env_var(norm) {
            self.env_map.contains_key(env_key)
        } else {
            self.vars.contains_key(norm)
        }
    }

    fn unset_var(
        &mut self,
        raw_name: &str,
        nocomplain: bool,
        loc: (&str, u32, u32),
    ) -> Result<(), EvalError> {
        let norm = normalize_var_name(raw_name);
        if let Some(env_key) = parse_env_var(norm) {
            if self.env_map.remove(env_key).is_none() && !nocomplain {
                return Err(EvalError::new(
                    "D0007",
                    format!("cannot unset undefined environment variable '::env({env_key})'"),
                    loc,
                ));
            }
            return Ok(());
        }
        if self.vars.remove(norm).is_none() && !nocomplain {
            return Err(EvalError::new(
                "D0007",
                format!("cannot unset undefined variable '{raw_name}'"),
                loc,
            ));
        }
        Ok(())
    }

    fn eval_script(
        &mut self,
        script: &str,
        file: &str,
        base_line: u32,
        base_col: u32,
        keep_going_on_cmd_err: bool,
    ) -> Result<TclVal, EvalError> {
        let cmds = parse_script_commands(script, file, base_line, base_col)?;
        let mut last = TclVal::Str(String::new());
        for cmd in cmds {
            let loc = (file, cmd.line, cmd.col);
            if self.step(loc).is_err() {
                let err = EvalError::new(
                    "D0008",
                    format!(
                        "evaluation step budget exceeded (limit: {} steps)",
                        self.limits.max_steps
                    ),
                    loc,
                );
                return Err(err);
            }
            match self.eval_parsed_cmd(&cmd, file, keep_going_on_cmd_err) {
                Ok(v) => last = v,
                Err(e) => {
                    if matches!(e.code, "D0006" | "D0008") || !keep_going_on_cmd_err {
                        return Err(e);
                    }
                    self.diags.push(e.into_diag());
                }
            }
        }
        Ok(last)
    }

    fn eval_parsed_cmd(
        &mut self,
        cmd: &ParsedCommand,
        file: &str,
        is_top_level: bool,
    ) -> Result<TclVal, EvalError> {
        let loc = (file, cmd.line, cmd.col);
        let mut argv = Vec::with_capacity(cmd.words.len());
        for w in &cmd.words {
            let val = self.eval_word(w, file)?;
            if w.expand {
                match val {
                    TclVal::Obj(o) => argv.push(TclVal::Obj(o)),
                    TclVal::List(items) => argv.extend(items),
                    TclVal::Str(s) => {
                        for elem in split_tcl_list(&s) {
                            argv.push(TclVal::Str(elem));
                        }
                    }
                    TclVal::SearchIndex { haystack, needle } => {
                        argv.push(TclVal::SearchIndex { haystack, needle });
                    }
                }
            } else {
                argv.push(val);
            }
        }
        if argv.is_empty() {
            return Ok(TclVal::Str(String::new()));
        }
        let cmd_name = argv[0].as_scalar_str().ok_or_else(|| {
            EvalError::new("D0002", "command name cannot be an object collection", loc)
        })?;
        let cmd_name = cmd_name.strip_prefix("::").unwrap_or(&cmd_name);
        let args = &argv[1..];
        self.dispatch_command(cmd_name, args, loc, is_top_level)
    }

    fn eval_word(&mut self, w: &WordAst, file: &str) -> Result<TclVal, EvalError> {
        let loc = (file, w.line, w.col);
        match &w.kind {
            WordKind::Braced(s) => Ok(TclVal::Str(s.clone())),
            WordKind::Segments(segs) => {
                if segs.is_empty() {
                    return Ok(TclVal::Str(String::new()));
                }
                if segs.len() == 1 {
                    match &segs[0] {
                        Segment::Lit(s) => return Ok(TclVal::Str(s.clone())),
                        Segment::Var(name) => return self.lookup_var(name, loc),
                        Segment::Cmd(sub, sub_line, sub_col) => {
                            return self.eval_nested_script(sub, (file, *sub_line, *sub_col));
                        }
                    }
                }
                let mut out = String::new();
                for seg in segs {
                    match seg {
                        Segment::Lit(s) => out.push_str(s),
                        Segment::Var(name) => {
                            let v = self.lookup_var(name, loc)?;
                            let s = v.as_scalar_str().ok_or_else(|| {
                                EvalError::new(
                                    "D0006",
                                    "interpolating an object collection into a string needs the netlist",
                                    loc,
                                )
                            })?;
                            out.push_str(&s);
                        }
                        Segment::Cmd(sub, sub_line, sub_col) => {
                            let v = self.eval_nested_script(sub, (file, *sub_line, *sub_col))?;
                            let s = v.as_scalar_str().ok_or_else(|| {
                                EvalError::new(
                                    "D0006",
                                    "interpolating an object collection into a string needs the netlist",
                                    loc,
                                )
                            })?;
                            out.push_str(&s);
                        }
                    }
                }
                Ok(TclVal::Str(out))
            }
        }
    }

    fn dispatch_command(
        &mut self,
        name: &str,
        args: &[TclVal],
        loc: (&str, u32, u32),
        is_top_level: bool,
    ) -> Result<TclVal, EvalError> {
        match name {
            "set" => {
                if args.len() == 1 {
                    let var = args[0].as_scalar_str().ok_or_else(|| {
                        EvalError::new("D0005", "'set' expects a variable name string", loc)
                    })?;
                    self.lookup_var(&var, loc)
                } else if args.len() == 2 {
                    let var = args[0].as_scalar_str().ok_or_else(|| {
                        EvalError::new("D0005", "'set' expects a variable name string", loc)
                    })?;
                    self.set_var(&var, args[1].clone(), loc)
                } else {
                    Err(EvalError::new(
                        "D0004",
                        format!("'set' expects 1 or 2 arguments, got {}", args.len()),
                        loc,
                    ))
                }
            }
            "unset" => {
                let mut idx = 0;
                let mut nocomplain = false;
                while idx < args.len() {
                    if let Some(tok) = args[idx].as_option_token() {
                        if tok == "-nocomplain" {
                            nocomplain = true;
                            idx += 1;
                        } else if tok == "--" {
                            idx += 1;
                            break;
                        } else {
                            return Err(EvalError::new(
                                "D0003",
                                format!("unknown option '{tok}' for 'unset'"),
                                loc,
                            ));
                        }
                    } else {
                        break;
                    }
                }
                for a in &args[idx..] {
                    let var = a.as_scalar_str().ok_or_else(|| {
                        EvalError::new("D0005", "'unset' expects variable name strings", loc)
                    })?;
                    self.unset_var(&var, nocomplain, loc)?;
                }
                Ok(TclVal::Str(String::new()))
            }
            "expr" => {
                if args.is_empty() {
                    return Err(EvalError::new(
                        "D0004",
                        "'expr' requires at least 1 argument",
                        loc,
                    ));
                }
                let mut parts = Vec::with_capacity(args.len());
                for a in args {
                    let s = a.as_scalar_str().ok_or_else(|| {
                        EvalError::new(
                            "D0006",
                            "expression over an object collection needs the netlist",
                            loc,
                        )
                    })?;
                    parts.push(s);
                }
                let expr_text = parts.join(" ");
                let res = eval_expr_str(self, &expr_text, loc)?;
                Ok(TclVal::Str(res))
            }
            "if" => self.eval_if(args, loc, is_top_level),
            "foreach" => self.eval_foreach(args, loc, is_top_level),
            "info" => self.eval_info(args, loc),
            "puts" => self.eval_puts(args, loc),
            "list" => {
                if args.iter().any(TclVal::contains_obj) {
                    let mut objs = Vec::with_capacity(args.len());
                    for a in args {
                        let o = a.to_obj().ok_or_else(|| {
                            EvalError::new(
                                "D0005",
                                "cannot mix non-object item into object collection list",
                                loc,
                            )
                        })?;
                        objs.push(o);
                    }
                    Ok(TclVal::Obj(Obj::union(objs)))
                } else {
                    Ok(TclVal::List(args.to_vec()))
                }
            }
            "concat" => {
                if args.iter().any(TclVal::contains_obj) {
                    let mut objs = Vec::new();
                    for a in args {
                        if let Some(o) = a.to_obj() {
                            objs.push(o);
                        }
                    }
                    if objs.is_empty() {
                        Ok(TclVal::List(Vec::new()))
                    } else {
                        Ok(TclVal::Obj(Obj::union(objs)))
                    }
                } else {
                    let mut flat = Vec::new();
                    for a in args {
                        let items = a.to_string_list().ok_or_else(|| {
                            EvalError::new("D0005", "invalid argument to 'concat'", loc)
                        })?;
                        for it in items {
                            flat.push(TclVal::Str(it));
                        }
                    }
                    Ok(TclVal::List(flat))
                }
            }
            "llength" => {
                if args.len() != 1 {
                    return Err(EvalError::new(
                        "D0004",
                        format!("'llength' expects 1 argument, got {}", args.len()),
                        loc,
                    ));
                }
                if args[0].contains_obj() {
                    return Err(EvalError::new(
                        "D0006",
                        "'llength' over an object collection needs the netlist",
                        loc,
                    ));
                }
                let items = args[0]
                    .to_string_list()
                    .ok_or_else(|| EvalError::new("D0005", "invalid list in 'llength'", loc))?;
                Ok(TclVal::Str(items.len().to_string()))
            }
            "lindex" => {
                if args.len() != 2 {
                    return Err(EvalError::new(
                        "D0004",
                        format!("'lindex' expects 2 arguments, got {}", args.len()),
                        loc,
                    ));
                }
                if args[0].contains_obj() {
                    return Err(EvalError::new(
                        "D0006",
                        "'lindex' over an object collection needs the netlist",
                        loc,
                    ));
                }
                let items = args[0]
                    .to_string_list()
                    .ok_or_else(|| EvalError::new("D0005", "invalid list in 'lindex'", loc))?;
                let idx_str = args[1]
                    .as_scalar_str()
                    .ok_or_else(|| EvalError::new("D0005", "'lindex' expects an index", loc))?;
                let idx = parse_list_index(&idx_str, items.len(), loc)?;
                let val = idx.and_then(|i| items.get(i).cloned()).unwrap_or_default();
                Ok(TclVal::Str(val))
            }
            "lappend" => {
                if args.is_empty() {
                    return Err(EvalError::new(
                        "D0004",
                        "'lappend' requires at least 1 argument",
                        loc,
                    ));
                }
                let var = args[0].as_scalar_str().ok_or_else(|| {
                    EvalError::new("D0005", "'lappend' expects a variable name", loc)
                })?;
                let current = self
                    .lookup_var(&var, loc)
                    .unwrap_or_else(|_| TclVal::List(Vec::new()));
                let any_obj = current.contains_obj() || args[1..].iter().any(TclVal::contains_obj);
                let next_val = if any_obj {
                    let mut objs = Vec::new();
                    if let Some(o) = current.to_obj() {
                        objs.push(o);
                    }
                    for a in &args[1..] {
                        if let Some(o) = a.to_obj() {
                            objs.push(o);
                        }
                    }
                    if objs.is_empty() {
                        TclVal::List(Vec::new())
                    } else {
                        TclVal::Obj(Obj::union(objs))
                    }
                } else {
                    let mut list = match current {
                        TclVal::List(items) => items,
                        TclVal::Str(s) => split_tcl_list(&s).into_iter().map(TclVal::Str).collect(),
                        _ => Vec::new(),
                    };
                    for a in &args[1..] {
                        list.push(a.clone());
                    }
                    TclVal::List(list)
                };
                self.set_var(&var, next_val, loc)
            }
            "split" => {
                if args.is_empty() || args.len() > 2 {
                    return Err(EvalError::new(
                        "D0004",
                        format!("'split' expects 1 or 2 arguments, got {}", args.len()),
                        loc,
                    ));
                }
                let s = args[0].as_scalar_str().ok_or_else(|| {
                    EvalError::new(
                        "D0006",
                        "'split' on an object collection needs the netlist",
                        loc,
                    )
                })?;
                let parts: Vec<TclVal> = if args.len() == 1 {
                    s.split_whitespace()
                        .map(|p| TclVal::Str(p.to_string()))
                        .collect()
                } else {
                    let chars = args[1].as_scalar_str().ok_or_else(|| {
                        EvalError::new("D0005", "'split' expects splitChars string", loc)
                    })?;
                    if chars.is_empty() {
                        s.chars().map(|c| TclVal::Str(c.to_string())).collect()
                    } else {
                        s.split(|c| chars.contains(c))
                            .map(|p| TclVal::Str(p.to_string()))
                            .collect()
                    }
                };
                Ok(TclVal::List(parts))
            }
            "join" => {
                if args.is_empty() || args.len() > 2 {
                    return Err(EvalError::new(
                        "D0004",
                        format!("'join' expects 1 or 2 arguments, got {}", args.len()),
                        loc,
                    ));
                }
                let items = args[0].to_string_list().ok_or_else(|| {
                    EvalError::new(
                        "D0006",
                        "'join' on an object collection needs the netlist",
                        loc,
                    )
                })?;
                let sep = if args.len() == 2 {
                    args[1].as_scalar_str().ok_or_else(|| {
                        EvalError::new("D0005", "'join' expects separator string", loc)
                    })?
                } else {
                    " ".to_string()
                };
                Ok(TclVal::Str(items.join(&sep)))
            }
            "string" => self.eval_string_cmd(args, loc),
            "add_to_collection" => {
                let mut idx = 0;
                while idx < args.len() {
                    if let Some(tok) = args[idx].as_option_token() {
                        if tok == "-unique" {
                            idx += 1;
                        } else {
                            return Err(EvalError::new(
                                "D0003",
                                format!("unknown option '{tok}' for 'add_to_collection'"),
                                loc,
                            ));
                        }
                    } else {
                        break;
                    }
                }
                if args.len() - idx != 2 {
                    return Err(EvalError::new(
                        "D0004",
                        "'add_to_collection' expects 2 collection arguments",
                        loc,
                    ));
                }
                let c1 = args[idx].to_obj().ok_or_else(|| {
                    EvalError::new("D0005", "'add_to_collection' expects collections", loc)
                })?;
                let c2 = args[idx + 1].to_obj().ok_or_else(|| {
                    EvalError::new("D0005", "'add_to_collection' expects collections", loc)
                })?;
                Ok(TclVal::Obj(Obj::union(vec![c1, c2])))
            }
            "remove_from_collection" => {
                if args.len() != 2 {
                    return Err(EvalError::new(
                        "D0004",
                        format!(
                            "'remove_from_collection' expects 2 arguments, got {}",
                            args.len()
                        ),
                        loc,
                    ));
                }
                let c1 = args[0].to_obj().ok_or_else(|| {
                    EvalError::new("D0005", "'remove_from_collection' expects collections", loc)
                })?;
                match args[1].to_obj() {
                    Some(c2) => Ok(TclVal::Obj(Obj::diff(c1, c2))),
                    None => Ok(TclVal::Obj(c1)),
                }
            }
            "lsearch" => self.eval_lsearch(args, loc),
            "lreplace" => self.eval_lreplace(args, loc),
            "source" => self.eval_source(args, loc),
            "proc" | "while" | "for" | "switch" | "eval" | "uplevel" | "upvar" | "global"
            | "variable" | "namespace" | "regexp" | "regsub" | "file" | "glob" | "open"
            | "close" | "gets" | "read" | "exec" | "catch" | "return" | "break" | "continue"
            | "array" | "dict" | "after" | "vwait" | "package" | "load" => Err(EvalError::new(
                "D0006",
                format!("Tcl command '{name}' is outside the deterministic SDC subset"),
                loc,
            )),
            _ => {
                // Check if this is an object query command (or `current_design` with 0 args / in nested brackets)
                if find_query_schema(name).is_some()
                    && (name != "current_design" || !is_top_level || args.is_empty())
                {
                    let obj = parse_query_cmd(name, args)
                        .map_err(|(code, msg)| EvalError::new(code, msg, loc))?;
                    return Ok(TclVal::Obj(obj));
                }
                // Otherwise dispatch as a top-level SDC constraint command
                let mut cmd_diags = Vec::new();
                if let Some(kind) = parse_top_cmd(
                    name,
                    args,
                    &mut self.active_units,
                    &mut self.sdc_units,
                    &mut self.current_design,
                    loc,
                    &mut cmd_diags,
                ) {
                    self.cmds
                        .push(Cmd::new(kind, self.current_source_idx, loc.1, loc.2));
                }
                self.diags.extend(cmd_diags);
                Ok(TclVal::Str(String::new()))
            }
        }
    }
}

mod builtins;
