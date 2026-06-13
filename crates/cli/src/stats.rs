//! Persistent-stats coprocess: the no-E pipeline's PYLINT_HOME stats pickle
//! (notes/09-pipeline-noE.md §5). prylint shells out to the embedded
//! stdlib-only `stats_helper.py` (python3 -I) for the two operations pylint
//! performs in full mode:
//!
//!   load <path>  -> previous_stats.global_note for the footer's
//!                   "(previous run: X.XX/10, +Y.YY)" suffix
//!                   (caching.load_results, swallow-all -> None).
//!   save <path>  -> a real pylint.utils.linterstats.LinterStats pickle so a
//!                   later pylint OR prylint run reads it back
//!                   (caching.save_results; gated on --persistent=yes).
//!
//! No pylint/astroid import is needed: save emits protocol-4 opcodes that name
//! the class by module/qualname (resolved in the reader's process); load uses
//! a stub-class Unpickler that only reads `global_note`.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::oracle; // reuse PRYLINT_PYTHON resolution

const HELPER_SRC: &str = include_str!("stats_helper.py");

fn embedded_helper_path() -> std::io::Result<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    HELPER_SRC.hash(&mut h);
    let dir = std::env::temp_dir();
    let path = dir.join(format!("prylint-stats-helper-{:016x}.py", h.finish()));
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if existing == HELPER_SRC {
            return Ok(path);
        }
    }
    let tmp = dir.join(format!(
        "prylint-stats-helper-{:016x}.{}.tmp",
        h.finish(),
        std::process::id()
    ));
    std::fs::write(&tmp, HELPER_SRC)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// PYLINT_HOME (constants.py:100-108): $PYLINTHOME if set, else
/// platformdirs.user_cache_dir("pylint") — macOS ~/Library/Caches/pylint,
/// Linux ~/.cache/pylint. Frozen at process start like the python module.
pub fn pylint_home() -> Option<PathBuf> {
    if let Ok(h) = std::env::var("PYLINTHOME") {
        return Some(PathBuf::from(h));
    }
    let home = std::env::var("HOME").ok()?;
    #[cfg(target_os = "macos")]
    {
        Some(PathBuf::from(home).join("Library/Caches/pylint"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Ok(x) = std::env::var("XDG_CACHE_HOME") {
            Some(PathBuf::from(x).join("pylint"))
        } else {
            Some(PathBuf::from(home).join(".cache/pylint"))
        }
    }
}

/// `_get_pdata_path` (caching.py:18-26): underscored Path(base_name).parts
/// (':','/','\\' -> '_'), joined by '_', suffixed `_1.stats` (recurs always 1).
/// `Path(".").parts == ()` -> "" -> "_1.stats".
pub fn pdata_path(home: &Path, base_name: &str) -> PathBuf {
    let parts = path_parts(base_name);
    let underscored: String = parts
        .iter()
        .map(|p| p.replace(':', "_").replace('/', "_").replace('\\', "_"))
        .collect::<Vec<_>>()
        .join("_");
    home.join(format!("{underscored}_1.stats"))
}

/// Emulate pathlib PurePath(...).parts for the base_name strings pylint feeds
/// (`expand_modules` basenames: dotted modnames like "pkg.a", or "." for the
/// cwd, or absolute paths). pylint constructs Path(base_name): a dotted name
/// has ONE part (no separator); "." has zero parts; "/abs/x" splits on '/'
/// with a leading '/' part.
fn path_parts(base_name: &str) -> Vec<String> {
    if base_name == "." || base_name.is_empty() {
        return Vec::new();
    }
    if base_name.starts_with('/') {
        let mut parts = vec!["/".to_string()];
        parts.extend(base_name.split('/').filter(|s| !s.is_empty()).map(|s| s.to_string()));
        return parts;
    }
    // Windows drive form "C:\x" -> ['C:', 'x'] under pathlib on posix is a
    // single part; we only need posix corpora here, so treat as one part.
    vec![base_name.to_string()]
}

fn helper_command() -> std::io::Result<Command> {
    let mut cmd = Command::new(oracle::oracle_python());
    cmd.arg("-I").arg(embedded_helper_path()?);
    Ok(cmd)
}

/// The stats values prylint persists (LinterStats __dict__ subset, §5.4). Only
/// `global_note` drives the footer suffix; the rest fills the pickle so real
/// pylint reads a well-formed LinterStats.
pub struct StatsSnapshot {
    pub global_note: Option<f64>,
    pub statement: u64,
    pub fatal: u64,
    pub error: u64,
    pub warning: u64,
    pub refactor: u64,
    pub convention: u64,
    pub info: u64,
    pub by_msg: Vec<(String, u64)>,
    pub modules_names: Vec<String>,
    pub module_count: u64,
}

pub struct StatsProc {
    proc: Option<(std::process::ChildStdin, BufReader<std::process::ChildStdout>)>,
    child: Option<std::process::Child>,
    failed: bool,
}

impl Default for StatsProc {
    fn default() -> Self {
        StatsProc { proc: None, child: None, failed: false }
    }
}

impl StatsProc {
    fn ensure(&mut self) -> bool {
        if self.proc.is_some() {
            return true;
        }
        if self.failed {
            return false;
        }
        let spawned = helper_command().and_then(|mut cmd| {
            cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn()
        });
        match spawned {
            Ok(mut child) => {
                let stdin = child.stdin.take().expect("piped stdin");
                let stdout = child.stdout.take().expect("piped stdout");
                self.proc = Some((stdin, BufReader::new(stdout)));
                self.child = Some(child);
                true
            }
            Err(_) => {
                self.failed = true;
                false
            }
        }
    }

    fn request(&mut self, req: serde_json::Value) -> Option<serde_json::Value> {
        if !self.ensure() {
            return None;
        }
        let (stdin, stdout) = self.proc.as_mut().unwrap();
        if writeln!(stdin, "{req}").is_err() || stdin.flush().is_err() {
            self.proc = None;
            return None;
        }
        let mut line = String::new();
        loop {
            line.clear();
            match stdout.read_line(&mut line) {
                Ok(0) | Err(_) => {
                    self.proc = None;
                    return None;
                }
                Ok(_) => {
                    if !line.trim().is_empty() {
                        break;
                    }
                }
            }
        }
        serde_json::from_str(line.trim()).ok()
    }

    /// `load_results(base).global_note` — None on missing/corrupt cache.
    pub fn load_global_note(&mut self, path: &Path) -> Option<f64> {
        let resp = self.request(
            serde_json::json!({"op":"load","path": path.to_string_lossy()}),
        )?;
        resp.get("global_note").and_then(|v| {
            if v.is_null() {
                None
            } else {
                v.as_f64()
            }
        })
    }

    /// `save_results(stats, base)` — writes a LinterStats pickle.
    pub fn save(&mut self, path: &Path, snap: &StatsSnapshot) {
        let by_msg: serde_json::Map<String, serde_json::Value> = snap
            .by_msg
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::json!(v)))
            .collect();
        let state = serde_json::json!({
            "global_note": snap.global_note,
            "statement": snap.statement,
            "fatal": snap.fatal,
            "error": snap.error,
            "warning": snap.warning,
            "refactor": snap.refactor,
            "convention": snap.convention,
            "info": snap.info,
            "skipped": 0,
            "nb_duplicated_lines": 0,
            "percent_duplicated_lines": 0.0,
            "by_msg": by_msg,
            "by_module": {},
            "dependencies": {},
            "bad_names": {},
            "code_type_count": {"code":0,"comment":0,"docstring":0,"empty":0,"total":0},
            "node_count": {"function":0,"klass":0,"method":0,"module": snap.module_count},
            "undocumented": {"function":0,"klass":0,"method":0,"module":0},
            "duplicated_lines": {"nb_duplicated_lines":0,"percent_duplicated_lines":0.0},
            "modules_names": {"__set__": snap.modules_names},
        });
        let _ = self.request(
            serde_json::json!({"op":"save","path": path.to_string_lossy(), "state": state}),
        );
    }
}

impl Drop for StatsProc {
    fn drop(&mut self) {
        self.proc = None;
        if let Some(mut c) = self.child.take() {
            let _ = c.wait();
        }
    }
}
