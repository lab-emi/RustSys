// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Run records, run ids and run references (PROTO-REQ-0050 to PROTO-REQ-0061).

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusteda_core::job::Diag;
use serde::{Deserialize, Serialize};

use crate::proto::Product;
use crate::proto::job::Job;

/// Value of `RunRecord::format`.
pub const RUN_FORMAT: &str = "rustsys-run";

/// State of a recorded run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    Running,
    Ok,
    Rejected,
    Internal,
    Negative,
    Undecided,
    Cancelled,
}

impl From<crate::proto::ExitClass> for RunStatus {
    fn from(c: crate::proto::ExitClass) -> RunStatus {
        match c {
            crate::proto::ExitClass::Ok => RunStatus::Ok,
            crate::proto::ExitClass::Rejected => RunStatus::Rejected,
            crate::proto::ExitClass::Internal => RunStatus::Internal,
            crate::proto::ExitClass::Negative => RunStatus::Negative,
            crate::proto::ExitClass::Undecided => RunStatus::Undecided,
            crate::proto::ExitClass::Cancelled => RunStatus::Cancelled,
        }
    }
}

/// Seconds of the parts of a run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Timing {
    pub load_s: f64,
    pub engine_s: f64,
    pub write_s: f64,
    pub total_s: f64,
}

/// An input file of a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRecord {
    pub role: String,
    pub path: PathBuf,
    pub bytes: u64,
    /// 64-bit FNV-1a of the contents, hexadecimal; empty when unreadable.
    pub fnv1a64: String,
}

/// An output file of a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputRecord {
    pub role: String,
    pub path: PathBuf,
}

/// Messages per severity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageCounts {
    pub error: usize,
    pub warning: usize,
    pub note: usize,
}

/// `run.json` (PROTO-REQ-0052).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub format: String,
    pub format_version: u32,
    pub id: String,
    pub product: Product,
    pub label: Option<String>,
    pub status: RunStatus,
    pub exit_status: Option<u8>,
    pub arguments: Vec<String>,
    pub command: String,
    pub cwd: PathBuf,
    pub job: Job,
    pub version: String,
    pub runtimes: Vec<String>,
    pub threads: usize,
    pub started: String,
    /// Start time in seconds since the Unix epoch, with fractions (orders runs of one second).
    #[serde(default)]
    pub started_s: f64,
    pub finished: Option<String>,
    pub timing: Timing,
    pub inputs: Vec<InputRecord>,
    pub outputs: Vec<OutputRecord>,
    /// Key figures, for lists (for example `area`, `mismatches`).
    pub summary: BTreeMap<String, f64>,
    pub diagnostics: MessageCounts,
}

impl RunRecord {
    /// An output path of the run, resolved against the run's working directory.
    pub fn output(&self, role: &str) -> Option<PathBuf> {
        self.outputs
            .iter()
            .find(|o| o.role == role)
            .map(|o| self.resolve(&o.path))
    }

    /// A recorded path resolved against the run's working directory (PROTO-REQ-0061).
    pub fn resolve(&self, p: &Path) -> PathBuf {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.cwd.join(p)
        }
    }
}

/// Seconds since the Unix epoch.
pub fn now_s() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Civil date (year, month, day) of a day number since 1970-01-01.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// UTC time parts of a Unix time.
fn utc(secs: f64) -> (i64, u32, u32, u32, u32, u32) {
    let s = secs.floor() as i64;
    let (y, mo, d) = civil(s.div_euclid(86_400));
    let r = s.rem_euclid(86_400);
    (
        y,
        mo,
        d,
        (r / 3600) as u32,
        ((r / 60) % 60) as u32,
        (r % 60) as u32,
    )
}

/// RFC 3339 UTC time, for example `2026-09-28T14:15:03Z`.
pub fn rfc3339(secs: f64) -> String {
    let (y, mo, d, h, mi, s) = utc(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// A run id for a start time (PROTO-REQ-0051).
pub fn run_id(product: Product, secs: f64, suffix: u16) -> String {
    let (y, mo, d, h, mi, s) = utc(secs);
    format!(
        "{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z-{}-{suffix:04x}",
        product.name()
    )
}

/// Creates a new run directory under `root` (PROTO-REQ-0051).
pub fn allocate(root: &Path, product: Product, secs: f64) -> Result<(String, PathBuf), Diag> {
    std::fs::create_dir_all(root)
        .map_err(|e| Diag::error("A0007", format!("cannot create {}: {e}", root.display())))?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let mut suffix = ((std::process::id() ^ nanos) & 0xffff) as u16;
    for _ in 0..0x10000 {
        let id = run_id(product, secs, suffix);
        let dir = root.join(&id);
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok((id, dir)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                suffix = suffix.wrapping_add(1);
            }
            Err(e) => {
                return Err(Diag::error(
                    "A0007",
                    format!("cannot create {}: {e}", dir.display()),
                ));
            }
        }
    }
    Err(Diag::error("A0007", "no free run id"))
}

/// Prepares `--run-dir DIR`: it must not hold a run yet (PROTO-REQ-0050).
pub fn prepare_dir(dir: &Path) -> Result<(), Diag> {
    if dir.join("run.json").exists() {
        return Err(
            Diag::error("A0007", format!("{} already holds a run", dir.display()))
                .with_help("give a new directory, or use --record for a fresh one"),
        );
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| Diag::error("A0007", format!("cannot create {}: {e}", dir.display())))
}

/// 64-bit FNV-1a of a file.
pub fn fnv1a64(path: &Path) -> Option<(u64, u64)> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut n = 0u64;
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let k = f.read(&mut buf).ok()?;
        if k == 0 {
            break;
        }
        n += k as u64;
        for &b in &buf[..k] {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    Some((h, n))
}

/// Records of the inputs of a job (PROTO-REQ-0052).
pub fn input_records(job: &Job) -> Vec<InputRecord> {
    job.inputs()
        .into_iter()
        .map(|(role, path)| {
            let (h, n) = fnv1a64(&path).unwrap_or((0, 0));
            InputRecord {
                role: role.into(),
                path,
                bytes: n,
                fnv1a64: if n == 0 && h == 0 {
                    String::new()
                } else {
                    format!("{h:016x}")
                },
            }
        })
        .collect()
}

/// Reads `dir/run.json`.
pub fn read_record(dir: &Path) -> Result<RunRecord, Diag> {
    let p = dir.join("run.json");
    let text = std::fs::read_to_string(&p)
        .map_err(|e| Diag::error("A0005", format!("{}: {e}", p.display())))?;
    serde_json::from_str(&text).map_err(|e| Diag::error("A0020", format!("{}: {e}", p.display())))
}

/// Runs under a runs root, newest first (by start time, then id).
pub fn list_runs(root: &Path) -> Vec<(PathBuf, RunRecord)> {
    let mut v: Vec<(PathBuf, RunRecord)> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter_map(|p| read_record(&p).ok().map(|r| (p, r)))
        .collect();
    // Newest first by the sub-second start time; records without it by their start string.
    v.sort_by(|a, b| {
        b.1.started_s
            .total_cmp(&a.1.started_s)
            .then(b.1.started.cmp(&a.1.started))
            .then(b.1.id.cmp(&a.1.id))
    });
    v
}

/// What a run reference names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunLocation {
    /// A run directory.
    Dir(PathBuf),
    /// A report file without a run record.
    Report(PathBuf),
}

/// Resolves a run reference (PROTO-REQ-0060).
pub fn resolve(reference: &str, root: Option<&Path>) -> Result<RunLocation, Diag> {
    let no_root = || {
        Diag::error("A0008", format!("'{reference}' needs a runs root"))
            .with_help("give --runs-root DIR or set RUSTSYS_RUNS_ROOT")
    };
    if let Some(rest) = reference.strip_prefix('@') {
        let root = root.ok_or_else(no_root)?;
        let (prod, back) = match rest.split_once('~') {
            Some((p, n)) => (
                p,
                n.parse::<usize>().map_err(|_| {
                    Diag::error("A0006", format!("'{reference}': ~ needs a number"))
                })?,
            ),
            None => (rest, 0),
        };
        let product = if prod.is_empty() {
            None
        } else {
            Some(Product::parse(prod).ok_or_else(|| {
                Diag::error("A0006", format!("'{reference}': unknown product '{prod}'"))
                    .with_help("products: synth, check-synth")
            })?)
        };
        let runs = list_runs(root);
        let pick = runs
            .iter()
            .filter(|(_, r)| product.is_none_or(|p| r.product == p))
            .nth(back);
        return match pick {
            Some((d, _)) => Ok(RunLocation::Dir(d.clone())),
            None => Err(Diag::error(
                "A0006",
                format!("no run '{reference}' under {}", root.display()),
            )),
        };
    }
    let p = Path::new(reference);
    if p.is_dir() {
        return Ok(RunLocation::Dir(p.to_path_buf()));
    }
    if p.is_file() {
        return Ok(RunLocation::Report(p.to_path_buf()));
    }
    if reference.len() >= 4 && !reference.contains('/') {
        let root = root.ok_or_else(no_root)?;
        let hits: Vec<(PathBuf, RunRecord)> = list_runs(root)
            .into_iter()
            .filter(|(_, r)| r.id.starts_with(reference))
            .collect();
        return match hits.len() {
            1 => Ok(RunLocation::Dir(hits[0].0.clone())),
            0 => Err(Diag::error(
                "A0006",
                format!("no run '{reference}' under {}", root.display()),
            )),
            _ => Err(Diag::error(
                "A0006",
                format!(
                    "'{reference}' is ambiguous: {}",
                    hits.iter()
                        .take(5)
                        .map(|h| h.1.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
        };
    }
    Err(Diag::error(
        "A0006",
        format!("no run or report '{reference}'"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_and_ids() {
        assert_eq!(rfc3339(0.0), "1970-01-01T00:00:00Z");
        // 2026-09-28T14:15:03Z
        let t = 1_790_604_903.0;
        assert_eq!(rfc3339(t), "2026-09-28T14:15:03Z");
        assert_eq!(
            run_id(Product::Synth, t, 0x3f9a),
            "20260928T141503Z-synth-3f9a"
        );
        assert_eq!(
            run_id(Product::CheckSynth, t, 0x0007),
            "20260928T141503Z-check-synth-0007"
        );
        assert_eq!(rfc3339(951_782_400.0), "2000-02-29T00:00:00Z");
    }

    // PROTO-REQ-0060
    #[test]
    fn references() {
        let root = std::env::temp_dir().join(format!("rustsys-refs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mk = |product: Product, t: f64, suffix: u16| {
            let id = run_id(product, t, suffix);
            let dir = root.join(&id);
            std::fs::create_dir_all(&dir).unwrap();
            let r = RunRecord {
                format: RUN_FORMAT.into(),
                format_version: 1,
                id: id.clone(),
                product,
                label: None,
                status: RunStatus::Ok,
                exit_status: Some(0),
                arguments: vec![],
                command: String::new(),
                cwd: root.clone(),
                job: match product {
                    Product::Synth => Job::Synth(Default::default()),
                    Product::CheckSynth => Job::CheckSynth(Default::default()),
                },
                version: "0".into(),
                runtimes: vec![],
                threads: 0,
                started: rfc3339(t),
                started_s: t,
                finished: None,
                timing: Timing::default(),
                inputs: vec![],
                outputs: vec![],
                summary: BTreeMap::new(),
                diagnostics: MessageCounts::default(),
            };
            crate::proto::write::write_json_atomic(&dir.join("run.json"), &r).unwrap();
            (id, dir)
        };
        let (_, s1) = mk(Product::Synth, 1000.0, 1);
        let (_, c1) = mk(Product::CheckSynth, 2000.0, 2);
        let (id3, s2) = mk(Product::Synth, 3000.0, 3);
        let r = Some(root.as_path());
        assert_eq!(resolve("@", r).unwrap(), RunLocation::Dir(s2.clone()));
        assert_eq!(resolve("@~1", r).unwrap(), RunLocation::Dir(c1.clone()));
        assert_eq!(resolve("@synth~1", r).unwrap(), RunLocation::Dir(s1));
        assert_eq!(resolve("@check-synth", r).unwrap(), RunLocation::Dir(c1));
        assert_eq!(resolve(&id3[..20], r).unwrap(), RunLocation::Dir(s2));
        assert_eq!(resolve("1970", r).unwrap_err().code, "A0006");
        assert_eq!(resolve("@", None).unwrap_err().code, "A0008");
        assert_eq!(resolve("@check-synth~9", r).unwrap_err().code, "A0006");
        assert_eq!(resolve("@bogus", r).unwrap_err().code, "A0006");
        let _ = std::fs::remove_dir_all(&root);
    }
}
