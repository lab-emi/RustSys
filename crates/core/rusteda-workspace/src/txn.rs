// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

use super::*;

/// The result of [`Txn::commit`].
#[derive(Debug, Clone)]
pub struct Committed {
    /// The new revision, now HEAD.
    pub rev: Rev,
    /// Its base (parent) revision.
    pub base: Rev,
    /// Its directory.
    pub dir: PathBuf,
    /// Its `meta.json`.
    pub meta: RevisionMeta,
    /// The tables carried from the base, by name.
    pub carried: Vec<String>,
    /// The history line of the commit.
    pub history: HistoryEntry,
    /// Problems after `manifest.json` was replaced (WS-REQ-0076): the revision is committed
    /// and HEAD, but the history line could not be written or the directory fsync failed. A
    /// command reports them as warnings of a successful result (`ok: true`, `new_rev` set).
    /// Empty normally.
    pub warnings: Vec<Diagnostic>,
}

/// A transaction: tables computed from a base revision, committed as a new revision.
///
/// Dropping a transaction without committing removes its staging directory.
#[derive(Debug)]
pub struct Txn {
    pub(super) ws: Workspace,
    pub(super) inv: Invocation,
    pub(super) base: Revision,
    pub(super) staging: PathBuf,
    pub(super) added: BTreeMap<String, TableEntry>,
    pub(super) unsynced: BTreeSet<String>,
    pub(super) libraries: Option<Vec<LibraryEntry>>,
    pub(super) sources: Vec<SourceEntry>,
    pub(super) drops: BTreeMap<String, String>,
    pub(super) message: Option<String>,
    pub(super) summary: TableValues,
    pub(super) status: TableValues,
    pub(super) exit: i32,
    pub(super) finished: bool,
}

impl Txn {
    /// The base revision.
    pub fn base(&self) -> &Revision {
        &self.base
    }

    /// The invocation.
    pub fn invocation(&self) -> &Invocation {
        &self.inv
    }

    /// The staging directory, `revisions/.staging-<pid>-<nanos>/`.
    pub fn staging_dir(&self) -> &Path {
        &self.staging
    }

    /// Where to write the file or directory of a new table: `path` is a single entry name of
    /// `[A-Za-z0-9._-]`, not starting with `.`, not `meta.json` (WS-REQ-0072).
    ///
    /// The files there must be written by the tool (a copy, a reflink copy included, is a new
    /// file) or be hard links to files of the base revision: a file that shares its inode with
    /// anything else, such as a source file of the user, would let an edit of that file
    /// rewrite the committed revision, so [`Txn::add_file`] and [`Txn::add_dir`] refuse it
    /// (WS-REQ-0077).
    pub fn staging_path(&self, path: &str) -> Result<PathBuf> {
        check_table_path(path)?;
        Ok(self.staging.join(path))
    }

    /// Adds a table whose file the tool wrote at `staging_path(path)`; the crate computes its
    /// file hash (WS-REQ-0011). A file with a second hard link to anything but a file of the
    /// base revision is `INTERNAL` (WS-REQ-0077). If the file is an rbin file (it starts with
    /// the magic), it must pass the structural checks of every open and its content hash
    /// (WS-REQ-0078), and its header must record `upstream` (WS-REQ-0038, see
    /// [`RbinWriter::upstream_tables`]); otherwise `INTERNAL`. The file is read once.
    pub fn add_file(&mut self, name: &str, path: &str, upstream: Upstream) -> Result<Hash64> {
        self.check_new(name, path, &upstream)?;
        let full = self.staging.join(path);
        let meta = fs::symlink_metadata(&full).map_err(|e| Error::io(&full, e))?;
        if !meta.is_file() {
            return Err(Error::internal(format!(
                "{}: not a regular file",
                full.display()
            )));
        }
        self.check_links(&full, &meta, &mut None)?;
        let file = fsutil::open_regular(&full)?;
        let len = file.metadata().map_err(|e| Error::io(&full, e))?.len();
        let inspected = rbin::inspect(file, len).map_err(|e| Error::io(&full, e))?;
        let hash = check_import(name, inspected, &upstream)?;
        self.insert(name, path, hash, upstream, true);
        Ok(hash)
    }

    /// Adds a directory table (e.g. an `rtl/` snapshot) the tool wrote at `staging_path(path)`;
    /// the crate computes its directory hash (WS-REQ-0012). The directory holds regular files
    /// and directories only, and no file with a second hard link to anything but a file of
    /// the base revision (WS-REQ-0077); otherwise `INTERNAL`.
    pub fn add_dir(&mut self, name: &str, path: &str, upstream: Upstream) -> Result<Hash64> {
        self.check_new(name, path, &upstream)?;
        let full = self.staging.join(path);
        let meta = fs::symlink_metadata(&full).map_err(|e| Error::io(&full, e))?;
        if !meta.is_dir() {
            return Err(Error::internal(format!(
                "{}: not a directory",
                full.display()
            )));
        }
        let files = hash::walk_files(&full).map_err(|e| match e {
            WalkError::Io(path, e) => Error::io(path, e),
            WalkError::Special { path, .. } | WalkError::NotUtf8 { path, .. } => {
                Error::internal(format!(
                    "{}: a directory table holds regular files and directories with UTF-8 names \
                     only",
                    path.display()
                ))
            }
        })?;
        let mut base_files = None;
        for (_, file) in &files {
            let meta = fs::symlink_metadata(file).map_err(|e| Error::io(file, e))?;
            self.check_links(file, &meta, &mut base_files)?;
        }
        let hash = hash::dir_hash(&hash_walked(&files)?);
        self.insert(name, path, hash, upstream, true);
        Ok(hash)
    }

    /// WS-REQ-0077: a staging file shares its inode with nothing but a file of the base
    /// revision, so that no name outside the revisions reaches the bytes of a committed table.
    /// `base_files` caches the (device, inode) pairs of the base revision's files; they are
    /// listed only when a file has more than one link.
    #[cfg(unix)]
    fn check_links(
        &self,
        file: &Path,
        meta: &fs::Metadata,
        base_files: &mut Option<BTreeSet<(u64, u64)>>,
    ) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() <= 1 {
            return Ok(());
        }
        let ids = match base_files {
            Some(ids) => ids,
            None => base_files.insert(fsutil::file_ids_below(&self.base.dir)?),
        };
        if ids.contains(&(meta.dev(), meta.ino())) {
            return Ok(());
        }
        Err(Error::internal(format!(
            "{}: the file has {} hard links and is not a file of the base revision {}; a \
             committed table must not share its file with a file outside the revisions, whose \
             edits would change it: copy the file into the staging directory (a reflink copy \
             is fine) instead of linking it",
            file.display(),
            meta.nlink(),
            self.base.rev
        )))
    }

    /// Without hard-link counts (not Unix), every staging file is taken as the tool's own.
    #[cfg(not(unix))]
    fn check_links(
        &self,
        _file: &Path,
        _meta: &fs::Metadata,
        _base_files: &mut Option<BTreeSet<(u64, u64)>>,
    ) -> Result<()> {
        Ok(())
    }

    /// Adds a table from bytes, written to `staging_path(path)` and fsynced. Bytes of an rbin
    /// file (they start with the magic) must pass the structural checks of every open and
    /// their content hash (WS-REQ-0078), and record `upstream` in their header (WS-REQ-0038);
    /// otherwise `INTERNAL`.
    pub fn add_bytes(
        &mut self,
        name: &str,
        path: &str,
        bytes: &[u8],
        upstream: Upstream,
    ) -> Result<Hash64> {
        self.check_new(name, path, &upstream)?;
        let inspected = rbin::inspect(bytes, bytes.len() as u64)
            .map_err(|e| Error::internal(format!("reading bytes in memory: {e}")))?;
        let hash = check_import(name, inspected, &upstream)?;
        fsutil::write_new(&self.staging.join(path), bytes)?;
        self.insert(name, path, hash, upstream, false);
        Ok(hash)
    }

    /// Adds an rbin table, written to `staging_path(path)` and fsynced. Its header records
    /// `upstream` (WS-REQ-0038): the writer's own upstream entries are left empty and filled
    /// here, or they must be exactly those of [`RbinWriter::upstream_tables`]`(&upstream)`;
    /// anything else is `INTERNAL`.
    pub fn add_rbin(
        &mut self,
        name: &str,
        path: &str,
        writer: &RbinWriter<'_>,
        upstream: Upstream,
    ) -> Result<Hash64> {
        self.check_new(name, path, &upstream)?;
        let entries = rbin::header_upstream(&upstream)?;
        let own = writer.upstream_entries();
        if !own.is_empty() && *own != entries {
            return Err(Error::internal(format!(
                "the rbin header entries of table {name} are not its upstream map; leave them out \
                 (add_rbin writes them) or set them with RbinWriter::upstream_tables"
            )));
        }
        let hash = writer.write_with_upstream(&self.staging.join(path), &entries)?;
        self.insert(name, path, hash, upstream, false);
        Ok(hash)
    }

    fn check_new(&self, name: &str, path: &str, upstream: &Upstream) -> Result<()> {
        check_table_name(name)?;
        check_table_path(path)?;
        for up in upstream.keys() {
            // WS-REQ-0013, WS-REQ-0014: `libraries` and `libraries.<corner>` are pseudo-tables.
            if !is_libraries_table(up) {
                check_table_name(up)?;
            }
            if up == name {
                return Err(Error::internal(format!(
                    "table {name} lists itself as upstream"
                )));
            }
        }
        if self.added.contains_key(name) {
            return Err(Error::internal(format!("table {name} is added twice")));
        }
        if self.drops.contains_key(name) {
            return Err(Error::internal(format!(
                "table {name} was dropped in this transaction"
            )));
        }
        if let Some((other, _)) = self.added.iter().find(|(_, t)| t.path == path) {
            return Err(Error::internal(format!(
                "path {path} is already table {other}"
            )));
        }
        Ok(())
    }

    fn insert(&mut self, name: &str, path: &str, hash: Hash64, upstream: Upstream, sync: bool) {
        let entry = TableEntry {
            path: path.to_string(),
            hash,
            upstream,
        };
        self.added.insert(name.to_string(), entry);
        if sync {
            self.unsynced.insert(name.to_string());
        }
    }

    /// Drops a table of the base revision explicitly; it is listed in `dropped` with `reason`.
    pub fn drop_table(&mut self, name: &str, reason: impl Into<String>) -> Result<()> {
        if self.added.contains_key(name) {
            return Err(Error::internal(format!(
                "table {name} was added in this transaction"
            )));
        }
        if !self.base.meta.tables.contains_key(name) {
            return Err(Error::MissingTable {
                rev: self.base.rev,
                table: name.to_string(),
            });
        }
        self.drops.insert(name.to_string(), reason.into());
        Ok(())
    }

    /// The libraries of the new revision: those set in this transaction, else the base's.
    pub fn libraries(&self) -> &[LibraryEntry] {
        self.libraries
            .as_deref()
            .unwrap_or(&self.base.meta.libraries)
    }

    /// Replaces the libraries (the `libraries` pseudo-table; WS-REQ-0073).
    pub fn set_libraries(&mut self, libraries: Vec<LibraryEntry>) -> Result<()> {
        for (i, library) in libraries.iter().enumerate() {
            check_library(library)?;
            let twin = libraries[..i]
                .iter()
                .any(|l| l.name == library.name && l.corner == library.corner);
            if twin {
                return Err(Error::internal(format!(
                    "library {} appears twice in corner {}",
                    library.name, library.corner
                )));
            }
        }
        self.libraries = Some(libraries);
        Ok(())
    }

    /// Adds a library, replacing one with the same name and corner.
    pub fn add_library(&mut self, library: LibraryEntry) -> Result<()> {
        check_library(&library)?;
        let mut libraries = self.libraries().to_vec();
        match libraries
            .iter_mut()
            .find(|l| l.name == library.name && l.corner == library.corner)
        {
            Some(slot) => *slot = library,
            None => libraries.push(library),
        }
        self.libraries = Some(libraries);
        Ok(())
    }

    /// Records a source file of a table added in this transaction, with its path as given and
    /// its file hash (WS-REQ-0074). A path that starts with `env:`, the prefix of environment
    /// values ([`Txn::add_env_source`]), is `INPUT_REJECTED`: name such a file `./env:...`
    /// (WS-REQ-0047).
    pub fn add_source(&mut self, table: &str, path: impl Into<String>, hash: Hash64) -> Result<()> {
        let path = path.into();
        if path.starts_with(ENV_SOURCE_PREFIX) {
            return Err(Error::input_rejected(format!(
                "the source path '{path}' starts with '{ENV_SOURCE_PREFIX}', which marks an \
                 environment value in meta.json; name the file ./{path}"
            )));
        }
        self.sources.push(SourceEntry {
            table: table.to_string(),
            path,
            hash,
        });
        Ok(())
    }

    /// Records environment value `name`=`value` that table `table`, added in this
    /// transaction, was computed with, such as `--sdc-env NAME=VALUE` of `rusteda import
    /// --sdc` (contract §3, CA-SDCENV): a source with the path `env:NAME=VALUE` and the hash
    /// of the value (WS-REQ-0047, [`SourceEntry::env_value`]). A name that is empty or holds
    /// `=`, or a NUL character in either part, is `INPUT_REJECTED`.
    pub fn add_env_source(&mut self, table: &str, name: &str, value: &str) -> Result<()> {
        self.sources
            .push(SourceEntry::env_value(table, name, value)?);
        Ok(())
    }

    /// Sets the message of the new revision.
    pub fn set_message(&mut self, message: impl Into<String>) {
        self.message = Some(message.into());
    }

    /// Sets summary value `key` of table `table`, which this transaction added (WS-REQ-0045,
    /// contract §3 CA-STATUS). A table's summary describes that table: it is carried forward
    /// with the table, copied from the base, and dropped with it, and its numeric values give
    /// the metric deltas of the history (WS-REQ-0044). Any other table is `INTERNAL`.
    pub fn set_summary(
        &mut self,
        table: &str,
        key: impl Into<String>,
        value: impl Into<Value>,
    ) -> Result<()> {
        self.check_added(table)?;
        self.summary
            .entry(table.to_string())
            .or_default()
            .insert(key.into(), value.into());
        Ok(())
    }

    /// Sets status value `key` of table `table`, which this transaction added (WS-REQ-0045,
    /// contract §3 CA-STATUS). The status is carried and dropped with its table, so it never
    /// outlives the table it describes. A property is recorded under the table of the command
    /// that checks it (CA-STATUS: equivalence under `check`, timing under `sta`): a property
    /// whose table a revision lacks is unchecked there, and `rusteda show` renders it so, so an
    /// "unchecked" value is not recorded under another table, where a later check could not
    /// replace it. Any other table is `INTERNAL`.
    pub fn set_status(
        &mut self,
        table: &str,
        key: impl Into<String>,
        value: impl Into<Value>,
    ) -> Result<()> {
        self.check_added(table)?;
        self.status
            .entry(table.to_string())
            .or_default()
            .insert(key.into(), value.into());
        Ok(())
    }

    fn check_added(&self, table: &str) -> Result<()> {
        if self.added.contains_key(table) {
            return Ok(());
        }
        Err(Error::internal(format!(
            "summary and status values describe a table this transaction adds; {table} is not one \
             (a carried table keeps the values of the base, contract §3 CA-STATUS)"
        )))
    }

    /// Sets the exit code of the command that commits (WS-REQ-0046): 0 (the default), 3 for a
    /// negative verdict or 5 for an inconclusive one. The history line records it with
    /// `ok: true`; any other code is `INTERNAL`.
    pub fn set_exit(&mut self, code: i32) -> Result<()> {
        if !matches!(code, exit::OK | exit::NEGATIVE_VERDICT | exit::INCONCLUSIVE) {
            return Err(Error::internal(format!(
                "a command that commits exits {}, {} or {}, not {code}",
                exit::OK,
                exit::NEGATIVE_VERDICT,
                exit::INCONCLUSIVE
            )));
        }
        self.exit = code;
        Ok(())
    }

    /// The hash table `name` will have in the new revision as the transaction stands: an added
    /// table, a libraries pseudo-table (`libraries`, `libraries.<corner>`), or a base table
    /// that carry-forward keeps.
    pub fn table_hash(&self, name: &str) -> Option<Hash64> {
        if let Some(hash) = libraries_table_hash(name, self.libraries()) {
            return hash;
        }
        if let Some(t) = self.added.get(name) {
            return Some(t.hash);
        }
        let (carried, _) = carry_forward(
            &self.base.meta.tables,
            &self.added,
            &self.drops,
            self.libraries(),
        );
        carried.get(name).map(|t| t.hash)
    }

    /// The upstream map of a new table computed from `names`, with their current hashes
    /// ([`Txn::table_hash`]). A table absent from the base is [`Error::MissingTable`]; one that
    /// the transaction drops is [`Error::DroppedTable`] with the reason (both `MISSING_TABLE`,
    /// WS-REQ-0075).
    pub fn upstream(&self, names: &[&str]) -> Result<Upstream> {
        let mut upstream = Upstream::new();
        for &name in names {
            let Some(hash) = self.table_hash(name) else {
                return Err(self.missing(name));
            };
            upstream.insert(name.to_string(), hash);
        }
        Ok(upstream)
    }

    /// Why table `name` has no hash in the new revision: absent from the base, or dropped
    /// (explicitly or by carry-forward); for `libraries.<corner>`, the corner has no library.
    fn missing(&self, name: &str) -> Error {
        if let Some(corner) = name.strip_prefix(CORNER_LIBRARIES_PREFIX) {
            if LibraryEntry::corner_hash(&self.base.meta.libraries, corner).is_none() {
                return Error::MissingTable {
                    rev: self.base.rev,
                    table: name.to_string(),
                };
            }
            return Error::DroppedTable {
                table: name.to_string(),
                reason: format!("no library of the new revision is in corner {corner}"),
            };
        }
        if !self.base.has_table(name) {
            return Error::MissingTable {
                rev: self.base.rev,
                table: name.to_string(),
            };
        }
        let (_, dropped) = carry_forward(
            &self.base.meta.tables,
            &self.added,
            &self.drops,
            self.libraries(),
        );
        let reason = dropped
            .into_iter()
            .find(|d| d.table == name)
            .map_or_else(|| "dropped".to_string(), |d| d.reason);
        Error::DroppedTable {
            table: name.to_string(),
            reason,
        }
    }

    /// Commits the transaction as a new revision and makes it HEAD (WS-REQ-0062 to
    /// WS-REQ-0066). On `REVISION_CONFLICT` the staging directory is deleted and the conflict is
    /// logged (`ok: false`, exit 4); any other failure before the commit point is logged too.
    /// After the commit point (the rename of `manifest.json`) the result is `Ok`, with
    /// [`Committed::warnings`] for a failed directory fsync or history append (WS-REQ-0076).
    /// `metrics` go into the history line, with the exit code of [`Txn::set_exit`].
    pub fn commit(mut self, metrics: JsonMap) -> Result<Committed> {
        self.finished = true;
        // From the immutable base chain, before the exclusive lock.
        let deltas = self.commit_deltas();
        let result = self.commit_inner(metrics, deltas);
        if result.is_err() {
            // Gone already if the failure came after the rename of the staging directory.
            fsutil::remove_dir_best_effort(&self.staging);
        }
        result
    }

    fn commit_inner(&mut self, metrics: JsonMap, deltas: TableValues) -> Result<Committed> {
        let root = self.ws.root.clone();
        let base = self.base.rev;
        let _lock = WorkspaceLock::exclusive(&root)?;
        let manifest = read_manifest(&root)?;
        let published = if manifest.head == base {
            self.check_staging().and_then(|()| self.publish(&manifest))
        } else {
            Err(Error::RevisionConflict {
                base,
                head: manifest.head,
            })
        };
        let published = match published {
            Ok(published) => published,
            Err(e) => {
                // Nothing is published: HEAD is still manifest.head.
                fsutil::remove_dir_best_effort(&self.staging);
                let line = history_line(
                    &self.inv,
                    Some(base),
                    None,
                    manifest.head,
                    Err(&e),
                    JsonMap::new(),
                );
                let _ = append_history(&root, &line);
                return Err(e);
            }
        };
        // WS-REQ-0076: the manifest rename was the commit point; nothing below fails the
        // commit, and the history line says what happened.
        let rev = published.rev;
        let what = format!("{rev} is committed and is HEAD");
        let mut warnings = Vec::new();
        if let Replaced::NotDurable(e) = published.manifest {
            warnings.push(not_durable(&what, layout::MANIFEST, &e));
        }
        let mut line = history_line(
            &self.inv,
            Some(base),
            Some(rev),
            rev,
            Ok(self.exit),
            metrics,
        );
        line.metric_deltas = deltas;
        if let Err(e) = append_history(&root, &line) {
            warnings.push(history_not_written(&what, &e));
        }
        Ok(Committed {
            rev,
            base,
            dir: self.ws.revision_dir(rev),
            meta: published.meta,
            carried: published.carried,
            history: line,
            warnings,
        })
    }

    /// The staging directory holds exactly the added tables (WS-REQ-0072).
    fn check_staging(&self) -> Result<()> {
        let paths: BTreeSet<&str> = self.added.values().map(|t| t.path.as_str()).collect();
        let entries = fs::read_dir(&self.staging).map_err(|e| Error::io(&self.staging, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| Error::io(&self.staging, e))?;
            let name = entry.file_name();
            if !paths.contains(name.to_string_lossy().as_ref()) {
                return Err(Error::internal(format!(
                    "{} is not a table of this transaction",
                    entry.path().display()
                )));
            }
        }
        for (name, table) in &self.added {
            if fs::symlink_metadata(self.staging.join(&table.path)).is_err() {
                return Err(Error::internal(format!(
                    "the file of table {name} disappeared"
                )));
            }
        }
        Ok(())
    }

    /// Steps of the commit protocol under the exclusive lock, up to the manifest update.
    fn publish(&self, manifest: &Manifest) -> Result<Published> {
        let revisions = self.ws.path(layout::REVISIONS);
        // WS-REQ-0063: skip numbers of directories left by an interrupted commit, and set
        // those directories aside.
        let existing = rev_dirs(&revisions)?;
        let (rev, next_rev) = allocate_rev(manifest.next_rev, existing.last().map(|(r, _)| *r))?;
        let orphans: Vec<(Rev, String)> = existing
            .into_iter()
            .filter(|(r, _)| r.0 >= manifest.next_rev)
            .collect();
        set_aside(&revisions, &orphans)?;

        // WS-REQ-0064: carry-forward to a fixpoint; WS-REQ-0071: new tables' upstreams hold.
        let libraries = self.libraries().to_vec();
        let (carried, dropped) =
            carry_forward(&self.base.meta.tables, &self.added, &self.drops, &libraries);
        for (name, table) in &self.added {
            for (up, want) in &table.upstream {
                let have = upstream_hash(up, &libraries, &self.added, &carried);
                if have != Some(*want) {
                    let have = have.map_or("no such table".to_string(), |h| h.to_string());
                    return Err(Error::internal(format!(
                        "table {name} was computed from {up} {want}, but the new revision has \
                         {have}"
                    )));
                }
            }
        }

        // WS-REQ-0065: hard-link the carried tables.
        for table in carried.values() {
            let src = self.base.dir.join(&table.path);
            let dst = self.staging.join(&table.path);
            if fs::symlink_metadata(&dst).is_ok() {
                return Err(Error::internal(format!(
                    "path {} is used by a new table and by a carried one",
                    table.path
                )));
            }
            fsutil::link_or_copy(&src, &dst)?;
        }

        // WS-REQ-0074: sources of carried tables, then those of new tables, by table name.
        let mut sources: Vec<SourceEntry> = self
            .base
            .meta
            .sources
            .iter()
            .filter(|s| carried.contains_key(&s.table))
            .cloned()
            .collect();
        for source in &self.sources {
            if !self.added.contains_key(&source.table) {
                return Err(Error::internal(format!(
                    "source {} names table {}, which this transaction did not add",
                    source.path, source.table
                )));
            }
            sources.push(source.clone());
        }
        sources.sort_by(|a, b| a.table.cmp(&b.table));

        // WS-REQ-0045, CA-STATUS: carried tables keep their summary and status, copied from the
        // base; added tables have those this transaction set; dropped tables lose theirs.
        let per_table = |base: &TableValues, own: &TableValues| -> TableValues {
            let mut values: TableValues = base
                .iter()
                .filter(|(table, _)| carried.contains_key(*table))
                .map(|(table, v)| (table.clone(), v.clone()))
                .collect();
            values.extend(own.iter().map(|(table, v)| (table.clone(), v.clone())));
            values
        };
        let summary = per_table(&self.base.meta.summary, &self.summary);
        let status = per_table(&self.base.meta.status, &self.status);

        let carried_names: Vec<String> = carried.keys().cloned().collect();
        let mut tables = carried;
        tables.extend(self.added.iter().map(|(n, t)| (n.clone(), t.clone())));
        let meta = RevisionMeta {
            schema: REVISION_SCHEMA.to_string(),
            rev,
            parent: Some(self.base.rev),
            created_unix_ms: fsutil::now_unix_ms(),
            tool: self.inv.tool.clone(),
            tool_version: self.inv.tool_version.clone(),
            command: self.inv.command.clone(),
            argv: self.inv.argv.clone(),
            message: self.message.clone(),
            tables,
            libraries,
            sources,
            dropped,
            summary,
            status,
        };

        // WS-REQ-0066: durable files and directories, then the renames.
        let text = schema::to_pretty_json(&meta)?;
        fsutil::write_new(&self.staging.join(layout::META), text.as_bytes())?;
        for name in &self.unsynced {
            fsutil::fsync_tree(&self.staging.join(&self.added[name].path))?;
        }
        fsutil::fsync_dirs(&self.staging)?;
        let target = revisions.join(rev.dir_name());
        if fs::symlink_metadata(&target).is_ok() {
            return Err(Error::internal(format!(
                "{} exists already",
                target.display()
            )));
        }
        fsutil::rename(&self.staging, &target)?;
        fsutil::fsync_dir(&revisions)?;
        let corners = LibraryEntry::corners(&meta.libraries);
        let manifest = write_manifest(&self.ws.root, &Manifest::new(rev, next_rev, corners))?;
        Ok(Published {
            rev,
            meta,
            carried: carried_names,
            manifest,
        })
    }

    /// The metric deltas of the commit (WS-REQ-0044): for every added table with a summary,
    /// its numeric values minus those of the table's previous version.
    fn commit_deltas(&self) -> TableValues {
        let mut deltas = TableValues::new();
        for (table, summary) in &self.summary {
            let Some(previous) = self.previous_summary(table) else {
                continue;
            };
            let changes = metric_deltas(summary, &previous);
            if !changes.is_empty() {
                deltas.insert(table.clone(), changes);
            }
        }
        deltas
    }

    /// The summary of the previous version of `table`: the one in the nearest revision on the
    /// parent chain from the base that has the table, at most [`DELTA_LOOKBACK`] revisions
    /// back. So `ruststa run` after an optimization that dropped the timing table still compares
    /// with the timing table before it.
    fn previous_summary(&self, table: &str) -> Option<JsonMap> {
        let summary_of = |meta: &RevisionMeta| meta.summary.get(table).cloned().unwrap_or_default();
        if self.base.meta.tables.contains_key(table) {
            return Some(summary_of(&self.base.meta));
        }
        let mut parent = self.base.meta.parent;
        for _ in 1..DELTA_LOOKBACK {
            // Committed revisions are immutable; one that cannot be read ends the search.
            let ancestor = self.ws.load_revision(parent?).ok()?;
            if ancestor.meta.tables.contains_key(table) {
                return Some(summary_of(&ancestor.meta));
            }
            parent = ancestor.meta.parent;
        }
        None
    }

    /// Abandons the transaction and removes its staging directory, read-only subdirectories
    /// included (WS-REQ-0079). A directory that cannot be removed stays behind as a
    /// `.staging-*` entry, which readers ignore (WS-REQ-0070).
    pub fn abort(mut self) -> Result<()> {
        self.finished = true;
        fsutil::remove_tree(&self.staging).map_err(|e| Error::io(&self.staging, e))
    }

    /// Abandons the transaction because the command failed with `error`: records the rejected
    /// command in the history (WS-REQ-0069), then removes the staging directory as
    /// [`Txn::abort`] does. The line is written first, so a staging directory that cannot be
    /// removed (a file still open on NFS) does not cost the command its line; it stays behind,
    /// ignored by readers (WS-REQ-0079). The error is that of the history append.
    pub fn reject(self, error: &Error) -> Result<HistoryEntry> {
        let line = self.ws.log_rejected(&self.inv, Some(self.base.rev), error);
        let _ = self.abort();
        line
    }
}

impl Drop for Txn {
    fn drop(&mut self) {
        if !self.finished {
            fsutil::remove_dir_best_effort(&self.staging);
        }
    }
}

/// What [`Txn::publish`] did: the new revision, and how `manifest.json` was replaced.
struct Published {
    rev: Rev,
    meta: RevisionMeta,
    carried: Vec<String>,
    manifest: Replaced,
}

/// The number of a new revision and the `next_rev` after it (WS-REQ-0063): `N = max(next_rev,
/// 1 + highest)`, where `highest` is the highest revision directory, so that the number of a
/// directory left by an interrupted commit is not used again. A stray directory near the top
/// of the `u32` range, which no commit creates, cannot be skipped that way: then `N` is
/// `next_rev`, and the directories at or above it are set aside all the same. Only a `next_rev`
/// that cannot advance is exhaustion (`INTERNAL`).
fn allocate_rev(next_rev: u32, highest: Option<Rev>) -> Result<(Rev, u32)> {
    let skipping = highest.map_or(Some(next_rev), |h| {
        h.0.checked_add(1).map(|n| n.max(next_rev))
    });
    [skipping, Some(next_rev)]
        .into_iter()
        .flatten()
        .find_map(|n| Some((Rev(n), n.checked_add(1)?)))
        .ok_or_else(|| Error::internal("revision numbers are exhausted"))
}

/// The hash upstream name `up` has in a new revision: a libraries pseudo-table, an added table
/// or a carried one.
fn upstream_hash(
    up: &str,
    libraries: &[LibraryEntry],
    added: &BTreeMap<String, TableEntry>,
    carried: &BTreeMap<String, TableEntry>,
) -> Option<Hash64> {
    match libraries_table_hash(up, libraries) {
        Some(hash) => hash,
        None => added.get(up).or_else(|| carried.get(up)).map(|t| t.hash),
    }
}

/// Carry-forward (WS-REQ-0064): the base tables that the transaction neither replaced nor
/// dropped survive while every upstream hash equals the hash of that table (or libraries
/// pseudo-table) in the new revision; dropping a table can invalidate others, so this repeats
/// to a fixpoint. Returns the carried tables and the dropped ones (explicit drops first, then
/// by round and name).
pub(super) fn carry_forward(
    base: &BTreeMap<String, TableEntry>,
    added: &BTreeMap<String, TableEntry>,
    explicit: &BTreeMap<String, String>,
    libraries: &[LibraryEntry],
) -> (BTreeMap<String, TableEntry>, Vec<DroppedEntry>) {
    let mut dropped: Vec<DroppedEntry> = explicit
        .iter()
        .map(|(table, reason)| DroppedEntry {
            table: table.clone(),
            reason: reason.clone(),
        })
        .collect();
    let mut carried: BTreeMap<String, TableEntry> = base
        .iter()
        .filter(|(name, _)| !added.contains_key(*name) && !explicit.contains_key(*name))
        .map(|(name, table)| (name.clone(), table.clone()))
        .collect();
    loop {
        let mut round = Vec::new();
        for (name, table) in &carried {
            for (up, want) in &table.upstream {
                let reason = match upstream_hash(up, libraries, added, &carried) {
                    Some(h) if h == *want => continue,
                    Some(_) => format!("upstream {up} changed"),
                    None => format!("upstream {up} is not in this revision"),
                };
                round.push(DroppedEntry {
                    table: name.clone(),
                    reason,
                });
                break;
            }
        }
        if round.is_empty() {
            return (carried, dropped);
        }
        for entry in round {
            carried.remove(&entry.table);
            dropped.push(entry);
        }
    }
}

/// Metric deltas between two revisions (WS-REQ-0044): for every table of both whose hash
/// differs, the numeric summary values of `new` minus those of `old`.
pub(super) fn changed_table_deltas(new: &RevisionMeta, old: &RevisionMeta) -> TableValues {
    let mut deltas = TableValues::new();
    for (name, table) in &new.tables {
        let Some(previous) = old.tables.get(name) else {
            continue;
        };
        if previous.hash == table.hash {
            continue;
        }
        let (Some(after), Some(before)) = (new.summary.get(name), old.summary.get(name)) else {
            continue;
        };
        let changes = metric_deltas(after, before);
        if !changes.is_empty() {
            deltas.insert(name.clone(), changes);
        }
    }
    deltas
}

/// The table hash of a file table entering a revision (WS-REQ-0078, WS-REQ-0038): an rbin file
/// must be valid and record its upstream map in its header; anything else is `INTERNAL`.
fn check_import(name: &str, inspected: Inspected, upstream: &Upstream) -> Result<Hash64> {
    if !inspected.records_upstream(&rbin::header_upstream(upstream)?) {
        return Err(Error::internal(format!(
            "the rbin header of table {name} does not record its upstream map; write the table \
             with Txn::add_rbin, or set the entries with RbinWriter::upstream_tables"
        )));
    }
    match inspected {
        Inspected::Plain(hash)
        | Inspected::Rbin {
            file_hash: hash, ..
        } => Ok(hash),
        Inspected::Corrupt(what) => Err(Error::internal(format!(
            "table {name} is an rbin file that fails validation: {what}"
        ))),
    }
}

/// Table names are handles and not the `libraries` pseudo-table (WS-REQ-0072).
pub(super) fn check_table_name(name: &str) -> Result<()> {
    if !is_handle(name) || name == LIBRARIES {
        return Err(Error::internal(format!(
            "table name '{name}' must match [A-Za-z0-9_-]+ and not be '{LIBRARIES}'"
        )));
    }
    Ok(())
}

/// Whether `path` is a table path: a single entry of the revision directory of
/// `[A-Za-z0-9._-]`, not starting with `.` (so never `.` or `..`), not `meta.json`
/// (WS-REQ-0024, WS-REQ-0072).
pub(super) fn is_table_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('.')
        && path != layout::META
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Table paths are single entries of the revision directory (WS-REQ-0072).
pub(super) fn check_table_path(path: &str) -> Result<()> {
    if !is_table_path(path) {
        return Err(Error::internal(format!(
            "table path '{path}' must be one name of [A-Za-z0-9._-], not starting with '.', \
             not '{}'",
            layout::META
        )));
    }
    Ok(())
}

/// What is wrong with a library entry (WS-REQ-0073): a name or corner (input), or a compiled
/// path that is not the library's cache path (a bug of the tool).
enum LibraryIssue {
    Input(String),
    Compiled(String),
}

fn library_issue(library: &LibraryEntry) -> Option<LibraryIssue> {
    if !cache::is_library_name(&library.name) {
        return Some(LibraryIssue::Input(format!(
            "library name '{}' must be 1 to 128 characters of [A-Za-z0-9._+-], not starting \
             with '.'",
            library.name
        )));
    }
    if !is_handle(&library.corner) {
        return Some(LibraryIssue::Input(format!(
            "corner '{}' of library {} must match [A-Za-z0-9_-]+",
            library.corner, library.name
        )));
    }
    let compiled = cache::rlib_path(&library.name, library.hash);
    (library.compiled != compiled).then(|| {
        LibraryIssue::Compiled(format!(
            "library {} records its compiled form at '{}', not at its cache path '{compiled}'",
            library.name, library.compiled
        ))
    })
}

/// A library entry a transaction adds (WS-REQ-0073): a name of the cache and a corner that is
/// a handle (else `INPUT_REJECTED`), and the compiled form at its cache path
/// `cache/<name>-<h16>.rlib` (else `INTERNAL`: [`crate::CachedLibrary::entry`] makes the
/// entry).
fn check_library(library: &LibraryEntry) -> Result<()> {
    match library_issue(library) {
        None => Ok(()),
        Some(LibraryIssue::Input(message)) => Err(Error::input_rejected(message)),
        Some(LibraryIssue::Compiled(message)) => Err(Error::internal(format!(
            "{message}; make the entry with CachedLibrary::entry"
        ))),
    }
}

/// Why a library entry read from `meta.json` is malformed (WS-REQ-0073): what
/// [`check_library`] refuses, or a (name, corner) that `before` has already.
pub(super) fn library_problem(library: &LibraryEntry, before: &[LibraryEntry]) -> Option<String> {
    if let Some(LibraryIssue::Input(problem) | LibraryIssue::Compiled(problem)) =
        library_issue(library)
    {
        return Some(problem);
    }
    let twin = before
        .iter()
        .any(|l| l.name == library.name && l.corner == library.corner);
    twin.then(|| {
        format!(
            "library {} appears twice in corner {}",
            library.name, library.corner
        )
    })
}
