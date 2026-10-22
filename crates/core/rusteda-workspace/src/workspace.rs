// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! The workspace: layout, revisions, transactions, the commit protocol, revert and history.
//!
//! ```text
//! <ws>/manifest.json  workspace.lock  history.jsonl  ui_state.json (reserved)
//! <ws>/cache/<name>-<h16>.lib  <name>-<h16>.rlib
//! <ws>/revisions/rev<NNNN>/meta.json + tables     (.staging-* and .orphan-* are ignored)
//! ```
//!
//! A mutation runs in three steps (WS-REQ-0060 to WS-REQ-0066):
//!
//! 1. [`Workspace::begin`] reads the manifest under the shared lock and fails with
//!    `REVISION_CONFLICT` unless HEAD is the base revision; it creates
//!    `revisions/.staging-<pid>-<nanos>/`.
//! 2. The tool computes lock-free from the immutable base revision and adds its tables to the
//!    [`Txn`].
//! 3. [`Txn::commit`] takes the exclusive lock, checks HEAD again, allocates
//!    `N = max(next_rev, 1 + highest revision directory)`, carries the base's tables forward
//!    (hard links) unless an upstream changed, writes `meta.json`, fsyncs, renames the staging
//!    directory to `rev<NNNN>`, replaces `manifest.json` atomically, appends the history line
//!    and unlocks.
//!
//! The rename of `manifest.json` is the commit point (WS-REQ-0076). A failure before it leaves
//! HEAD where it was and is an error. After it the change has happened: a failed directory
//! fsync or history append is a warning of the result ([`Initialized::warnings`],
//! [`Committed::warnings`], [`Reverted::warnings`]), never an error, and no failure line is
//! written for it.
//!
//! Every workspace-changing command gets one history line, rejected and conflicted ones
//! included (WS-REQ-0069): [`Workspace::init`], [`Workspace::fork`], [`Txn::commit`] and
//! [`Workspace::revert`] log their outcome, success or failure, and [`Workspace::begin`] its
//! failure, so `ws.begin(base, &inv)?` loses no line. The caller logs only what fails outside
//! them: before `begin` (a command line or a `--base-rev` that does not resolve) with
//! [`Workspace::log_rejected`], and between `begin` and `commit` with [`Txn::reject`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::RangeInclusive;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::cache::{self, LibraryCache};
use crate::envelope::{Diagnostic, Severity};
use crate::error::{Corruption, Error, Result, codes, exit};
use crate::fsutil::{self, Replaced};
use crate::hash::{self, Hash64, WalkError};
use crate::lock::WorkspaceLock;
use crate::rbin::{self, Inspected, Rbin, RbinWriter};
use crate::rev::{BaseRev, Handle, Rev, RevSpec, is_handle};
use crate::schema::{
    self, CORNER_LIBRARIES_PREFIX, DroppedEntry, ENV_SOURCE_PREFIX, HISTORY_SCHEMA, HistoryEntry,
    HistoryError, JsonMap, LIBRARIES, LibraryEntry, MANIFEST_SCHEMA, Manifest, REVISION_SCHEMA,
    RevisionMeta, SourceEntry, TableEntry, TableValues, UI_STATE_SCHEMA, UiState, Upstream,
    is_libraries_table, libraries_table_hash, metric_deltas,
};

/// File and directory names of the workspace layout.
pub mod layout {
    /// The HEAD pointer.
    pub const MANIFEST: &str = "manifest.json";
    /// The lock file.
    pub const LOCK: &str = "workspace.lock";
    /// The audit log.
    pub const HISTORY: &str = "history.jsonl";
    /// The GUI selection (reserved).
    pub const UI_STATE: &str = "ui_state.json";
    /// The library cache.
    pub const CACHE: &str = "cache";
    /// The revisions.
    pub const REVISIONS: &str = "revisions";
    /// The metadata file of a revision.
    pub const META: &str = "meta.json";
    /// Prefix of transaction staging directories in `revisions/`.
    pub const STAGING_PREFIX: &str = ".staging-";
    /// Prefix of set-aside revision directories in `revisions/`.
    pub const ORPHAN_PREFIX: &str = ".orphan-";
}

/// How many revisions, the base included, a commit searches along the parent chain for the
/// previous version of a table it adds, to compute the table's metric deltas (WS-REQ-0044).
pub const DELTA_LOOKBACK: usize = 64;

/// Who changes the workspace, as recorded in `meta.json` and `history.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// The tool, e.g. `rusteda`.
    pub tool: String,
    /// Its version.
    pub tool_version: String,
    /// The command, e.g. `rusteda import`.
    pub command: String,
    /// The argument vector as it should be recorded (typically with `argv[0]` replaced by the
    /// tool name).
    pub argv: Vec<String>,
}

impl Invocation {
    /// An invocation from its parts.
    pub fn new<S: Into<String>>(
        tool: impl Into<String>,
        tool_version: impl Into<String>,
        command: impl Into<String>,
        argv: impl IntoIterator<Item = S>,
    ) -> Invocation {
        Invocation {
            tool: tool.into(),
            tool_version: tool_version.into(),
            command: command.into(),
            argv: argv.into_iter().map(Into::into).collect(),
        }
    }
}

/// An open workspace directory. Cheap to clone; every call reads the current manifest.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// Creates a workspace: `manifest.json` (HEAD `rev0`, `next_rev` 1), an empty
    /// `workspace.lock`, `cache/`, `revisions/rev0000/meta.json` (no tables) and the first
    /// `history.jsonl` line (WS-REQ-0001).
    ///
    /// Fails with `WORKSPACE_EXISTS` if `manifest.json` exists (WS-REQ-0002); the rejected
    /// command is logged in the existing workspace when its manifest can be read
    /// (WS-REQ-0069). Once `manifest.json` is written the workspace exists: a failed directory
    /// fsync or history append after that is a warning of the result (WS-REQ-0076).
    pub fn init(root: impl Into<PathBuf>, inv: &Invocation) -> Result<Initialized> {
        let ws = Workspace { root: root.into() };
        let exists = || Error::WorkspaceExists {
            path: ws.root.clone(),
        };
        if ws.path(layout::MANIFEST).exists() {
            let error = exists();
            // Best effort: a workspace whose manifest cannot be read takes no line.
            let _ = ws.log_rejected(inv, None, &error);
            return Err(error);
        }
        // WS-REQ-0066: the entries of the directories created here are durable before the
        // workspace exists, as the files inside it are.
        fsutil::create_dir_all_durable(&ws.root)?;
        let lock_path = ws.path(layout::LOCK);
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| Error::io(&lock_path, e))?;
        let _lock = WorkspaceLock::exclusive(&ws.root)?;
        if ws.path(layout::MANIFEST).exists() {
            let error = exists();
            let _ = ws.log_rejected_locked(inv, None, &error);
            return Err(error);
        }
        fsutil::create_dir_all(&ws.path(layout::CACHE))?;
        let revisions = ws.path(layout::REVISIONS);
        fsutil::create_dir_all(&revisions)?;
        // An interrupted init may have left revision directories: set them aside.
        set_aside(&revisions, &rev_dirs(&revisions)?)?;

        let meta = RevisionMeta {
            schema: REVISION_SCHEMA.to_string(),
            rev: Rev::ZERO,
            parent: None,
            created_unix_ms: fsutil::now_unix_ms(),
            tool: inv.tool.clone(),
            tool_version: inv.tool_version.clone(),
            command: inv.command.clone(),
            argv: inv.argv.clone(),
            message: None,
            tables: BTreeMap::new(),
            libraries: Vec::new(),
            sources: Vec::new(),
            dropped: Vec::new(),
            summary: TableValues::new(),
            status: TableValues::new(),
        };
        let staging = fsutil::create_unique_dir(&revisions, layout::STAGING_PREFIX)?;
        let staged = schema::to_pretty_json(&meta).and_then(|text| {
            fsutil::write_new(&staging.join(layout::META), text.as_bytes())?;
            fsutil::fsync_dir(&staging)?;
            fsutil::rename(&staging, &revisions.join(Rev::ZERO.dir_name()))
        });
        if let Err(e) = staged {
            fsutil::remove_dir_best_effort(&staging);
            return Err(e);
        }
        fsutil::fsync_dir(&revisions)?;
        let manifest = write_manifest(&ws.root, &Manifest::new(Rev::ZERO, 1, Vec::new()))?;

        // WS-REQ-0076: the directory is a workspace now; nothing below can undo that.
        let what = "the workspace is created";
        let mut warnings = Vec::new();
        if let Replaced::NotDurable(e) = manifest {
            warnings.push(not_durable(what, layout::MANIFEST, &e));
        }
        let line = history_line(
            inv,
            None,
            Some(Rev::ZERO),
            Rev::ZERO,
            Ok(exit::OK),
            JsonMap::new(),
        );
        if let Err(e) = append_history(&ws.root, &line) {
            warnings.push(history_not_written(what, &e));
        }
        Ok(Initialized {
            workspace: ws,
            history: line,
            warnings,
        })
    }

    /// Opens a workspace: `NOT_A_WORKSPACE` without `manifest.json` (WS-REQ-0003),
    /// `SCHEMA_TOO_NEW` for a newer manifest (WS-REQ-0004).
    pub fn open(root: impl Into<PathBuf>) -> Result<Workspace> {
        let ws = Workspace { root: root.into() };
        if !ws.path(layout::MANIFEST).is_file() {
            return Err(Error::NotAWorkspace { path: ws.root });
        }
        ws.manifest()?;
        Ok(ws)
    }

    /// The workspace directory, as given.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// The directory of a revision (whether or not it exists).
    pub fn revision_dir(&self, rev: Rev) -> PathBuf {
        self.path(layout::REVISIONS).join(rev.dir_name())
    }

    /// Reads `manifest.json` under the shared lock.
    pub fn manifest(&self) -> Result<Manifest> {
        let _lock = WorkspaceLock::shared(&self.root)?;
        read_manifest(&self.root)
    }

    /// The current HEAD.
    pub fn head(&self) -> Result<Rev> {
        Ok(self.manifest()?.head)
    }

    /// Resolves the revision of a query: `HEAD` or `rev<N>` (`--rev`, `--from`, `--to`).
    /// Unknown revisions are `UNKNOWN_REVISION` (WS-REQ-0021, WS-REQ-0023).
    pub fn resolve_query(&self, spec: &str) -> Result<Rev> {
        let parsed = RevSpec::parse(spec).map_err(|e| Error::UnknownRevision {
            spec: spec.to_string(),
            reason: e.reason,
        })?;
        let manifest = self.manifest()?;
        match parsed {
            RevSpec::Head => Ok(manifest.head),
            RevSpec::Rev(rev) => {
                self.check_visible(&manifest, rev, spec)?;
                Ok(rev)
            }
        }
    }

    /// Resolves the base revision of a mutation (`--base-rev`): only `rev<N>` of an existing
    /// revision; `HEAD` and unknown revisions are `UNKNOWN_REVISION` (WS-REQ-0021). Whether it
    /// is HEAD is checked by [`Workspace::begin`] and [`Workspace::revert`].
    pub fn resolve_base(&self, spec: &str) -> Result<BaseRev> {
        let base = BaseRev::parse(spec).map_err(|e| Error::UnknownRevision {
            spec: spec.to_string(),
            reason: e.reason,
        })?;
        let manifest = self.manifest()?;
        self.check_visible(&manifest, base.rev(), spec)?;
        Ok(base)
    }

    /// A revision exists for readers if its directory has `meta.json` and its number is below
    /// `next_rev` (WS-REQ-0023): directories of an interrupted commit stay invisible.
    fn check_visible(&self, manifest: &Manifest, rev: Rev, spec: &str) -> Result<()> {
        if rev.0 < manifest.next_rev && self.revision_dir(rev).join(layout::META).is_file() {
            return Ok(());
        }
        Err(Error::UnknownRevision {
            spec: spec.to_string(),
            reason: format!("no such revision (HEAD is {})", manifest.head),
        })
    }

    /// Opens a revision: its `meta.json` and its tables.
    pub fn revision(&self, rev: Rev) -> Result<Revision> {
        let manifest = self.manifest()?;
        self.check_visible(&manifest, rev, &rev.to_string())?;
        self.load_revision(rev)
    }

    /// Reads and checks the `meta.json` of a revision. Every table path must name one entry of
    /// the revision directory, and no two tables may share one (WS-REQ-0024): a path from
    /// `meta.json` never reaches a file outside the revision. Every library entry names its
    /// compiled form in the cache (WS-REQ-0073), and `summary` and `status` hold keys only for
    /// tables of the revision (CA-STATUS, WS-REQ-0045).
    fn load_revision(&self, rev: Rev) -> Result<Revision> {
        let dir = self.revision_dir(rev);
        let path = dir.join(layout::META);
        let bytes = fsutil::read_regular(&path)?;
        let meta: RevisionMeta = schema::parse_json(&path, &bytes, REVISION_SCHEMA)?;
        let malformed = |reason: String| Error::corrupt(&path, Corruption::Json { reason });
        if meta.rev != rev {
            return Err(malformed(format!("meta.json of {rev} names {}", meta.rev)));
        }
        let mut paths = BTreeSet::new();
        for (name, table) in &meta.tables {
            if !is_table_path(&table.path) {
                return Err(malformed(format!(
                    "table {name} has the path '{}', which is not one entry of the revision \
                     directory",
                    table.path
                )));
            }
            if !paths.insert(table.path.as_str()) {
                return Err(malformed(format!(
                    "two tables have the path '{}'",
                    table.path
                )));
            }
        }
        for (i, library) in meta.libraries.iter().enumerate() {
            if let Some(problem) = library_problem(library, &meta.libraries[..i]) {
                return Err(malformed(problem));
            }
        }
        if let Some(problem) = meta.values_problem() {
            return Err(malformed(problem));
        }
        Ok(Revision {
            root: self.root.clone(),
            rev,
            dir,
            meta,
        })
    }

    /// The revisions that exist, in increasing order.
    pub fn revisions(&self) -> Result<Vec<Rev>> {
        let manifest = self.manifest()?;
        let dirs = rev_dirs(&self.path(layout::REVISIONS))?;
        Ok(dirs
            .into_iter()
            .map(|(rev, _)| rev)
            .filter(|&rev| self.check_visible(&manifest, rev, "").is_ok())
            .collect())
    }

    /// Starts a transaction on `base`: under the shared lock, HEAD must be `base`, otherwise
    /// `REVISION_CONFLICT` before any work (WS-REQ-0060). Creates the staging directory
    /// `revisions/.staging-<pid>-<nanos>/` (WS-REQ-0061). A failure, the conflict included, is
    /// logged here as a rejected command with its base revision (WS-REQ-0069), so the caller
    /// does not log it again.
    pub fn begin(&self, base: BaseRev, inv: &Invocation) -> Result<Txn> {
        self.begin_unlogged(base, inv).inspect_err(|error| {
            // Best effort: a workspace whose manifest cannot be read takes no line.
            let _ = self.log_rejected(inv, Some(base.rev()), error);
        })
    }

    fn begin_unlogged(&self, base: BaseRev, inv: &Invocation) -> Result<Txn> {
        let head = self.head()?;
        if head != base.rev() {
            return Err(Error::RevisionConflict {
                base: base.rev(),
                head,
            });
        }
        let base_revision = self.load_revision(base.rev())?;
        let staging =
            fsutil::create_unique_dir(&self.path(layout::REVISIONS), layout::STAGING_PREFIX)?;
        Ok(Txn {
            ws: self.clone(),
            inv: inv.clone(),
            base: base_revision,
            staging,
            added: BTreeMap::new(),
            unsynced: BTreeSet::new(),
            libraries: None,
            sources: Vec::new(),
            drops: BTreeMap::new(),
            message: None,
            summary: TableValues::new(),
            status: TableValues::new(),
            exit: exit::OK,
            finished: false,
        })
    }

    /// Moves HEAD from `base` to the existing revision `to`: exclusive lock, HEAD check,
    /// manifest update and a history line; no revision is created (WS-REQ-0068). The outcome,
    /// success or failure, is logged. Once `manifest.json` is replaced HEAD has moved: a failed
    /// directory fsync or history append after that is a warning of the result (WS-REQ-0076).
    pub fn revert(&self, base: BaseRev, to: Rev, inv: &Invocation) -> Result<Reverted> {
        let _lock = WorkspaceLock::exclusive(&self.root)?;
        let manifest = read_manifest(&self.root)?;
        let (line, replaced) = match self.revert_locked(&manifest, base, to, inv) {
            Ok(done) => done,
            Err(e) => {
                // Nothing changed: HEAD is still manifest.head.
                let line = history_line(
                    inv,
                    Some(base.rev()),
                    None,
                    manifest.head,
                    Err(&e),
                    JsonMap::new(),
                );
                let _ = append_history(&self.root, &line);
                return Err(e);
            }
        };
        // WS-REQ-0076: HEAD has moved; nothing below can undo that.
        let what = format!("HEAD is {to}");
        let mut warnings = Vec::new();
        if let Replaced::NotDurable(e) = replaced {
            warnings.push(not_durable(&what, layout::MANIFEST, &e));
        }
        if let Err(e) = append_history(&self.root, &line) {
            warnings.push(history_not_written(&what, &e));
        }
        Ok(Reverted {
            previous: base.rev(),
            head: to,
            history: line,
            warnings,
        })
    }

    fn revert_locked(
        &self,
        manifest: &Manifest,
        base: BaseRev,
        to: Rev,
        inv: &Invocation,
    ) -> Result<(HistoryEntry, Replaced)> {
        if manifest.head != base.rev() {
            return Err(Error::RevisionConflict {
                base: base.rev(),
                head: manifest.head,
            });
        }
        self.check_visible(manifest, to, &to.to_string())?;
        let target = self.load_revision(to)?;
        // The old HEAD gives only the metric deltas: a damaged meta.json there must not block
        // the way back to a good revision.
        let from = self.load_revision(base.rev()).ok();
        let corners = LibraryEntry::corners(&target.meta.libraries);
        let replaced = write_manifest(&self.root, &Manifest::new(to, manifest.next_rev, corners))?;
        let mut line = history_line(
            inv,
            Some(base.rev()),
            None,
            to,
            Ok(exit::OK),
            JsonMap::new(),
        );
        if let Some(from) = from {
            line.metric_deltas = changed_table_deltas(&target.meta, &from.meta);
        }
        Ok((line, replaced))
    }

    /// Forks the workspace into `dst`, a new workspace that starts at this one's HEAD (plan
    /// §1.1: an experiment on its own copy; WS-REQ-0052). The copy is made under the shared
    /// lock, so it is one state of the workspace: HEAD and every revision below `next_rev`,
    /// with no commit half-way. Revision files and cached libraries are hard-linked, which is
    /// safe because no workspace ever writes them again (they are copied where links are
    /// impossible, as across file systems); `history.jsonl` and `ui_state.json` are copied;
    /// staging and orphan directories are left out. The source is not changed and gets no
    /// history line; the fork gets one of its own (`inv`).
    ///
    /// The fork is built aside, in a new directory `.<name>.fork-<pid>-<nanos>` next to `dst`,
    /// which writes its manifest and history line under the fork's exclusive lock, and is
    /// published by one rename onto `dst`, the commit point of a fork (WS-REQ-0053,
    /// WS-REQ-0076). So `dst` never holds a partial fork: a failure removes only the directory
    /// that this call built, and a crash leaves only that directory, which nothing reads and a
    /// retry does not need. `dst` must be a new directory or an empty one, which the fork's
    /// directory replaces (with the empty directory's permissions); its parent must be
    /// writable. A workspace at `dst` is `WORKSPACE_EXISTS`, anything else (a file, a
    /// non-empty directory, a mount point, a path that does not end in a name)
    /// `INPUT_REJECTED`, also when another fork or an `init` claims `dst` first: the rename
    /// then fails and leaves their files alone. A failed fsync of the directory that holds
    /// the published fork is a warning of the result.
    ///
    /// A plain copy of the directory (`cp -r`, `cp --reflink`) is consistent only if it holds
    /// a shared lock on `workspace.lock` meanwhile (`flock -s`), or copies `manifest.json`
    /// before `revisions/`.
    pub fn fork(&self, dst: impl Into<PathBuf>, inv: &Invocation) -> Result<Forked> {
        let fork = Workspace { root: dst.into() };
        let (parent, target) = fork_location(&fork.root)?;
        let empty_dir = check_fork_destination(&target)?;
        fsutil::create_dir_all_durable(&parent)?;
        let aside = fsutil::create_unique_dir(&parent, &fork_prefix(&target))?;
        if let Some(permissions) = empty_dir {
            // The fork's directory replaces the empty one: it keeps the permissions it had.
            let _ = fs::set_permissions(&aside, permissions);
        }
        let result = self.fork_aside(&aside, &target, fork, inv);
        if result.is_err() {
            // Nothing is published (or the rename failed): remove what this call built, and
            // nothing else.
            fsutil::remove_dir_best_effort(&aside);
        }
        result
    }

    /// Builds the fork in `aside`, a new directory of this call next to `target`, and
    /// publishes it by renaming `aside` onto `target` (WS-REQ-0053).
    fn fork_aside(
        &self,
        aside: &Path,
        target: &Path,
        fork: Workspace,
        inv: &Invocation,
    ) -> Result<Forked> {
        fsutil::write_new(&aside.join(layout::LOCK), b"")?;
        // The fork's exclusive lock, held until it is published: the lock file keeps its inode
        // through the rename, so a process that opens the fork at `target` meanwhile waits for
        // the fork to finish, as for an init.
        let lock = WorkspaceLock::exclusive(aside)?;
        let cache = aside.join(layout::CACHE);
        let revisions = aside.join(layout::REVISIONS);
        for dir in [&cache, &revisions] {
            fs::create_dir(dir).map_err(|e| Error::io(dir, e))?;
        }
        let manifest = self.copy_into(aside)?;
        #[cfg(test)]
        if fsutil::failpoint::FORK_AFTER_COPY.replace(false) {
            return Err(Error::io(aside, io::Error::other("injected fork failure")));
        }
        // WS-REQ-0066: every directory of the copy is durable before the manifest is written,
        // and the manifest and the history line before the fork is published.
        fsutil::fsync_dirs(aside)?;
        if let Replaced::NotDurable(e) = write_manifest(aside, &manifest)? {
            return Err(e);
        }
        let head = manifest.head;
        let line = history_line(inv, None, None, head, Ok(exit::OK), JsonMap::new());
        append_history(aside, &line)?;

        // WS-REQ-0076: the rename is the commit point; nothing below can undo it.
        publish_fork(aside, target)?;
        let mut warnings = Vec::new();
        if let Err(e) = fsutil::fsync_dir(fsutil::parent_dir(target)) {
            let what = format!("the fork is created at {head}");
            warnings.push(not_durable(&what, "its directory entry", &e));
        }
        drop(lock);
        Ok(Forked {
            workspace: fork,
            head,
            history: line,
            warnings,
        })
    }

    /// Copies this workspace into the new directory `dst` under the shared lock: the revisions
    /// below `next_rev`, the cached libraries, `history.jsonl` and `ui_state.json`. Returns
    /// the manifest the copy belongs to.
    fn copy_into(&self, dst: &Path) -> Result<Manifest> {
        let revisions = dst.join(layout::REVISIONS);
        let cache = dst.join(layout::CACHE);
        // Commits publish under the exclusive lock, so none lands during the copy.
        let _lock = WorkspaceLock::shared(&self.root)?;
        let manifest = read_manifest(&self.root)?;
        for (rev, name) in rev_dirs(&self.path(layout::REVISIONS))? {
            if self.check_visible(&manifest, rev, "").is_ok() {
                fsutil::link_or_copy(&self.revision_dir(rev), &revisions.join(name))?;
            }
        }
        let source = self.path(layout::CACHE);
        let entries = match fs::read_dir(&source) {
            Ok(entries) => Some(entries),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(Error::io(&source, e)),
        };
        for entry in entries.into_iter().flatten() {
            let entry = entry.map_err(|e| Error::io(&source, e))?;
            let name = entry.file_name();
            let regular = entry.file_type().is_ok_and(|t| t.is_file());
            // Temporary files of an atomic write start with a dot.
            if regular && !name.to_string_lossy().starts_with('.') {
                fsutil::link_or_copy(&entry.path(), &cache.join(&name))?;
            }
        }
        for file in [layout::HISTORY, layout::UI_STATE] {
            if let Some(bytes) = fsutil::read_regular_if_exists(&self.path(file))? {
                fsutil::write_new(&dst.join(file), &bytes)?;
            }
        }
        Ok(manifest)
    }

    /// Records a command rejected before it reached [`Workspace::begin`] (a command line or a
    /// `--base-rev` that does not resolve) with its base revision, if any, under the exclusive
    /// lock (WS-REQ-0069). `begin`, `commit`, `revert` and [`Txn::reject`] log their own
    /// failures.
    pub fn log_rejected(
        &self,
        inv: &Invocation,
        base: Option<Rev>,
        error: &Error,
    ) -> Result<HistoryEntry> {
        let _lock = WorkspaceLock::exclusive(&self.root)?;
        self.log_rejected_locked(inv, base, error)
    }

    /// [`Workspace::log_rejected`] for a caller that holds the exclusive lock.
    fn log_rejected_locked(
        &self,
        inv: &Invocation,
        base: Option<Rev>,
        error: &Error,
    ) -> Result<HistoryEntry> {
        let manifest = read_manifest(&self.root)?;
        let line = history_line(inv, base, None, manifest.head, Err(error), JsonMap::new());
        append_history(&self.root, &line)?;
        Ok(line)
    }

    /// Reads `history.jsonl`. A line that is not JSON (torn by a crash) is skipped; a newer
    /// major schema is `SCHEMA_TOO_NEW` (WS-REQ-0042).
    pub fn history(&self) -> Result<Vec<HistoryEntry>> {
        #[derive(Deserialize)]
        struct Probe {
            schema: Option<String>,
        }
        let path = self.path(layout::HISTORY);
        let Some(bytes) = fsutil::read_regular_if_exists(&path)? else {
            return Ok(Vec::new());
        };
        let mut entries = Vec::new();
        for (index, line) in bytes.split(|&b| b == b'\n').enumerate() {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let Ok(probe) = serde_json::from_slice::<Probe>(line) else {
                continue;
            };
            let line_no = index + 1;
            let Some(found) = probe.schema else {
                let reason = format!("line {line_no} has no \"schema\" key");
                return Err(Error::corrupt(path, Corruption::Json { reason }));
            };
            schema::check_schema(&path, &found, HISTORY_SCHEMA)?;
            let entry = serde_json::from_slice(line).map_err(|e| {
                let reason = format!("line {line_no}: {e}");
                Error::corrupt(&path, Corruption::Json { reason })
            })?;
            entries.push(entry);
        }
        Ok(entries)
    }

    /// The library cache of this workspace (WS-REQ-0080). A cache exists only for a workspace:
    /// its compiled libraries are mapped, which is sound only for files that the workspace
    /// never writes again (WS-REQ-0037).
    pub fn cache(&self) -> LibraryCache {
        LibraryCache::new(self.root.clone())
    }

    /// Reads `ui_state.json`, or `None` when there is none. The file comes from another
    /// program, so its handles are checked ([`UiState::validate`]): one that is not shell-safe
    /// makes the file `CORRUPT_TABLE` (WS-REQ-0043).
    pub fn ui_state(&self) -> Result<Option<UiState>> {
        let path = self.path(layout::UI_STATE);
        let Some(bytes) = fsutil::read_regular_if_exists(&path)? else {
            return Ok(None);
        };
        let state: UiState = schema::parse_json(&path, &bytes, UI_STATE_SCHEMA)?;
        state.validate().map_err(|e| {
            let reason = format!("a handle of the selection: {e}");
            Error::corrupt(&path, Corruption::Json { reason })
        })?;
        Ok(Some(state))
    }

    /// Replaces `ui_state.json` atomically (for the GUI; the command line only reads it). A
    /// handle that is not shell-safe is `INTERNAL` (WS-REQ-0043).
    pub fn write_ui_state(&self, state: &UiState) -> Result<()> {
        state
            .validate()
            .map_err(|e| Error::internal(format!("ui_state.json: {e}")))?;
        let text = schema::to_pretty_json(state)?;
        fsutil::write_atomic(&self.path(layout::UI_STATE), text.as_bytes())
    }

    /// Resolves `--target sel<N>` against `ui_state.json` for revision `rev` (WS-REQ-0043):
    /// `UNKNOWN_HANDLE` when the file is absent, holds another selection, or belongs to another
    /// revision.
    pub fn resolve_selection(&self, target: &str, rev: Rev) -> Result<UiState> {
        let unknown = |reason: String| Error::UnknownHandle {
            handle: target.to_string(),
            reason,
        };
        let index = Handle::parse_as(target, "sel").map_err(|e| unknown(e.reason))?;
        let Some(state) = self.ui_state()? else {
            return Err(unknown(
                "the workspace has no ui_state.json (selections are made in the GUI)".into(),
            ));
        };
        if Handle::parse_as(&state.selection_handle, "sel").ok() != Some(index) {
            return Err(unknown(format!(
                "ui_state.json holds selection '{}'",
                state.selection_handle
            )));
        }
        if state.revision != rev {
            return Err(unknown(format!(
                "{target} belongs to {}, not {rev}; pass --rev {} (or --base-rev {})",
                state.revision, state.revision, state.revision
            )));
        }
        Ok(state)
    }
}

/// The result of [`Workspace::init`].
#[derive(Debug, Clone)]
pub struct Initialized {
    /// The new workspace.
    pub workspace: Workspace,
    /// Its first history line.
    pub history: HistoryEntry,
    /// Problems after `manifest.json` was written (WS-REQ-0076): the workspace exists, but its
    /// history line could not be written or the directory fsync failed. A command reports them
    /// as warnings of a successful result. Empty normally.
    pub warnings: Vec<Diagnostic>,
}

/// The result of [`Workspace::fork`].
#[derive(Debug, Clone)]
pub struct Forked {
    /// The new workspace.
    pub workspace: Workspace,
    /// Its HEAD: the HEAD of the source when the copy was made.
    pub head: Rev,
    /// Its own history line, after those copied from the source.
    pub history: HistoryEntry,
    /// Problems after the fork's `manifest.json` was written (WS-REQ-0076): the fork exists,
    /// but its history line could not be written or the directory fsync failed. Empty
    /// normally.
    pub warnings: Vec<Diagnostic>,
}

/// The result of [`Workspace::revert`].
#[derive(Debug, Clone)]
pub struct Reverted {
    /// HEAD before.
    pub previous: Rev,
    /// HEAD after.
    pub head: Rev,
    /// The history line of the revert.
    pub history: HistoryEntry,
    /// Problems after `manifest.json` was replaced (WS-REQ-0076): HEAD has moved, but the
    /// history line could not be written or the directory fsync failed. Empty normally.
    pub warnings: Vec<Diagnostic>,
}

/// A committed revision: its `meta.json` and access to its tables.
#[derive(Debug, Clone)]
pub struct Revision {
    root: PathBuf,
    rev: Rev,
    dir: PathBuf,
    meta: RevisionMeta,
}

impl Revision {
    /// The revision.
    pub fn rev(&self) -> Rev {
        self.rev
    }

    /// Its directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Its `meta.json`.
    pub fn meta(&self) -> &RevisionMeta {
        &self.meta
    }

    /// Whether it has table `name`.
    pub fn has_table(&self, name: &str) -> bool {
        self.meta.tables.contains_key(name)
    }

    /// Table `name`, or `MISSING_TABLE`.
    pub fn table(&self, name: &str) -> Result<&TableEntry> {
        self.meta
            .tables
            .get(name)
            .ok_or_else(|| Error::MissingTable {
                rev: self.rev,
                table: name.to_string(),
            })
    }

    /// The path of table `name`, an entry of the revision directory (WS-REQ-0024).
    pub fn table_path(&self, name: &str) -> Result<PathBuf> {
        Ok(self.dir.join(&self.table(name)?.path))
    }

    /// The hash of table `name`, including the pseudo-tables `libraries` and
    /// `libraries.<corner>`.
    pub fn table_hash(&self, name: &str) -> Option<Hash64> {
        self.meta.table_hash(name)
    }

    /// The list hash of the libraries (WS-REQ-0013).
    pub fn libraries_hash(&self) -> Hash64 {
        LibraryEntry::list_hash(&self.meta.libraries)
    }

    /// The corner names of the libraries, in first-appearance order.
    pub fn corners(&self) -> Vec<String> {
        LibraryEntry::corners(&self.meta.libraries)
    }

    /// Maps rbin table `name`, validating its header (`kind`, `versions`; WS-REQ-0032). The
    /// table must be a regular file (WS-REQ-0025), and its header must record the table's
    /// upstream map from `meta.json` (WS-REQ-0038); otherwise `CORRUPT_TABLE`. Committed
    /// tables are never written again, so they are mapped, not read (WS-REQ-0037).
    pub fn open_rbin(&self, name: &str, kind: &str, versions: RangeInclusive<u32>) -> Result<Rbin> {
        let table = self.table(name)?;
        let path = self.dir.join(&table.path);
        let rbin = Rbin::map_immutable(&path, kind, versions)?;
        let recorded = rbin::header_upstream(&table.upstream)
            .is_ok_and(|expected| rbin.has_upstream(&expected));
        if !recorded {
            let what = Corruption::HeaderUpstream {
                table: name.to_string(),
            };
            return Err(Error::corrupt(path, what));
        }
        Ok(rbin)
    }

    /// Maps the compiled form of `library`, one of this revision's libraries, from the cache
    /// (`cache/<name>-<h16>.rlib`, WS-REQ-0082), validating its header (`kind`, `versions`). It
    /// must be a regular file, checked without following a symbolic link (WS-REQ-0025); a
    /// library that is not one of this revision's is `INTERNAL`.
    pub fn open_library(
        &self,
        library: &LibraryEntry,
        kind: &str,
        versions: RangeInclusive<u32>,
    ) -> Result<Rbin> {
        if !self.meta.libraries.contains(library) {
            return Err(Error::internal(format!(
                "library {} of corner {} is not a library of {}",
                library.name, library.corner, self.rev
            )));
        }
        // meta.json was checked when the revision was read: `compiled` is the cache path of
        // the library's name and hash (WS-REQ-0073).
        LibraryCache::new(self.root.clone()).open_rlib(&library.name, library.hash, kind, versions)
    }

    /// Reads a file table into memory (e.g. a small JSON table); it must be a regular file
    /// (WS-REQ-0025). A directory table is `INTERNAL`: its files are read with
    /// [`Revision::table_files`] and [`Revision::read_table_file`] (WS-REQ-0026).
    pub fn read_table(&self, name: &str) -> Result<Vec<u8>> {
        let path = self.table_path(name)?;
        if fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            return Err(Error::internal(format!(
                "table {name} of {} is a directory table: list its files with \
                 Revision::table_files and read one with Revision::read_table_file",
                self.rev
            )));
        }
        fsutil::read_regular(&path)
    }

    /// The files of directory table `name` (such as an `rtl/` snapshot) with their file
    /// hashes, sorted by relative path (`/` separators), after checking the directory hash
    /// against `meta.json` (WS-REQ-0026). Symbolic links are not followed: one, a special
    /// file, a name that is not UTF-8 or a hash that differs is `CORRUPT_TABLE`; a file table
    /// is `INTERNAL`.
    pub fn table_files(&self, name: &str) -> Result<Vec<(String, Hash64)>> {
        let entry = self.table(name)?;
        let path = self.dir_table(name, &entry.path)?;
        let files = dir_table_hashes(&path)?;
        check_table_hash(name, &path, entry.hash, hash::dir_hash(&files))?;
        Ok(files)
    }

    /// Reads file `file` of directory table `name`: a relative path with `/` separators, as
    /// [`Revision::table_files`] lists it (WS-REQ-0026). Every directory on the way must be a
    /// directory and the file a regular file, without following symbolic links; otherwise
    /// `CORRUPT_TABLE`. A path that is not a plain relative path (empty components, `.`,
    /// `..`) is `INTERNAL`, and a file the table does not have is `IO_ERROR`.
    pub fn read_table_file(&self, name: &str, file: &str) -> Result<Vec<u8>> {
        let entry = self.table(name)?;
        let mut path = self.dir_table(name, &entry.path)?;
        let parts: Vec<&str> = file.split('/').collect();
        // Characters that also separate path components on this platform.
        const SEPARATORS: &[char] = if cfg!(windows) { &['\\', ':'] } else { &[] };
        let plain = |part: &&str| {
            !part.is_empty()
                && *part != "."
                && *part != ".."
                && !part.contains('\0')
                && !part.contains(SEPARATORS)
        };
        if !parts.iter().all(plain) {
            return Err(Error::internal(format!(
                "'{file}' is not a relative path of a file of table {name}"
            )));
        }
        let (last, dirs) = parts.split_last().unwrap_or((&"", &[]));
        for part in dirs {
            path.push(part);
            let meta = fs::symlink_metadata(&path).map_err(|e| Error::io(&path, e))?;
            if meta.file_type().is_file() {
                let source = io::Error::from(io::ErrorKind::NotADirectory);
                return Err(Error::io(&path, source));
            }
            if !meta.is_dir() {
                let what = Corruption::FileType {
                    expected: "a regular file or a directory",
                };
                return Err(Error::corrupt(&path, what));
            }
        }
        path.push(last);
        fsutil::read_regular(&path)
    }

    /// The directory of directory table `name` at `rel`: `INTERNAL` for a file table,
    /// `CORRUPT_TABLE` for anything that is neither.
    fn dir_table(&self, name: &str, rel: &str) -> Result<PathBuf> {
        let path = self.dir.join(rel);
        let meta = fs::symlink_metadata(&path).map_err(|e| Error::io(&path, e))?;
        if meta.is_dir() {
            return Ok(path);
        }
        if meta.file_type().is_file() {
            return Err(Error::internal(format!(
                "table {name} of {} is a file table: read it with Revision::read_table or \
                 Revision::open_rbin",
                self.rev
            )));
        }
        let what = Corruption::FileType {
            expected: "a regular file or a directory",
        };
        Err(Error::corrupt(path, what))
    }

    /// Recomputes the file or directory hash of table `name` and compares it with
    /// `meta.json`; a difference, a table that is neither a regular file nor a directory, or
    /// in a directory table an entry that is neither (a symbolic link, a FIFO), or a name that
    /// is not UTF-8, is `CORRUPT_TABLE` (WS-REQ-0025).
    pub fn verify_table(&self, name: &str) -> Result<()> {
        let entry = self.table(name)?;
        let path = self.dir.join(&entry.path);
        let meta = fs::symlink_metadata(&path).map_err(|e| Error::io(&path, e))?;
        let computed = if meta.is_dir() {
            hash::dir_hash(&dir_table_hashes(&path)?)
        } else if meta.file_type().is_file() {
            hash::hash_reader(fsutil::open_regular(&path)?).map_err(|e| Error::io(&path, e))?
        } else {
            let what = Corruption::FileType {
                expected: "a regular file or a directory",
            };
            return Err(Error::corrupt(path, what));
        };
        check_table_hash(name, &path, entry.hash, computed)
    }
}

/// The files of a committed directory table with their hashes (WS-REQ-0012): an entry that is
/// not a regular file or a directory, or a name that is not UTF-8, is `CORRUPT_TABLE`, since a
/// revision never holds one; a failed read is `IO_ERROR`.
fn dir_table_hashes(dir: &Path) -> Result<Vec<(String, Hash64)>> {
    let files = hash::walk_files(dir).map_err(|e| match e {
        WalkError::Io(path, e) => Error::io(path, e),
        WalkError::Special { path, .. } => Error::corrupt(
            path,
            Corruption::FileType {
                expected: "a regular file or a directory",
            },
        ),
        WalkError::NotUtf8 { path, .. } => Error::corrupt(path, Corruption::NotUtf8),
    })?;
    hash_walked(&files)
}

/// The file hashes of walked files, each opened as a regular file only (WS-REQ-0025).
fn hash_walked(files: &[(String, PathBuf)]) -> Result<Vec<(String, Hash64)>> {
    files
        .iter()
        .map(|(rel, path)| {
            let hash =
                hash::hash_reader(fsutil::open_regular(path)?).map_err(|e| Error::io(path, e))?;
            Ok((rel.clone(), hash))
        })
        .collect()
}

/// `CORRUPT_TABLE` unless the hash of table `name` on disk is the one `meta.json` records.
fn check_table_hash(name: &str, path: &Path, stored: Hash64, computed: Hash64) -> Result<()> {
    if computed == stored {
        return Ok(());
    }
    let what = Corruption::TableHash {
        table: name.to_string(),
        stored,
        computed,
    };
    Err(Error::corrupt(path, what))
}

#[path = "txn.rs"]
mod txn;
#[path = "workspace_io.rs"]
mod workspace_io;

use txn::*;
pub use txn::{Committed, Txn};
use workspace_io::*;
