//! Unified startup python driver (Win B of the startup-latency work).
//!
//! prylint needs a python interpreter at startup for four independent probes:
//!   (i)   default-config-file discovery,
//!   (ii)  the interpreter sys.path (+ builtins/stdlib/ext-suffixes/frozen),
//!   (iii) executing --init-hook / rcfile init-hook and capturing its sys.path
//!         delta,
//!   (iv)  loading (and later saving) the PYLINT_HOME stats pickle for the
//!         score footer's "previous run" suffix.
//!
//! Historically these were FOUR-plus separate `python -I` spawns (config-helper
//! even ran TWICE — discover then parse). This module replaces them with ONE
//! persistent `python -I` coprocess running `startup_driver.py`: a single
//! interpreter boot serves every request over a JSON stdin/stdout loop (the
//! same protocol the syntax-oracle uses). Ordering/semantics are unchanged —
//! the Rust caller issues the requests in the same order the old code did
//! (config discover -> parse -> probe -> init-hook -> stats load/save).
//!
//! The driver is a lazily-spawned process-global singleton so the same
//! interpreter that did config discovery at the top of main() is still alive at
//! the bottom of run() to write the stats pickle — no second spawn.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, OnceLock};

use crate::oracle; // reuse PRYLINT_PYTHON resolution

const DRIVER_SRC: &str = include_str!("startup_driver.py");

/// Materialize the embedded driver to a stable content-keyed temp path
/// (idempotent, race-free) so `python -I` can run it without a repo checkout.
fn embedded_driver_path() -> std::io::Result<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    DRIVER_SRC.hash(&mut h);
    let dir = std::env::temp_dir();
    let path = dir.join(format!("prylint-startup-driver-{:016x}.py", h.finish()));
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if existing == DRIVER_SRC {
            return Ok(path);
        }
    }
    let tmp = dir.join(format!(
        "prylint-startup-driver-{:016x}.{}.tmp",
        h.finish(),
        std::process::id()
    ));
    std::fs::write(&tmp, DRIVER_SRC)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// The single persistent startup coprocess.
pub struct StartupDriver {
    proc: Option<(ChildStdin, BufReader<ChildStdout>)>,
    child: Option<Child>,
    failed: bool,
}

impl StartupDriver {
    fn new() -> Self {
        StartupDriver { proc: None, child: None, failed: false }
    }

    fn ensure(&mut self) -> bool {
        if self.proc.is_some() {
            return true;
        }
        if self.failed {
            return false;
        }
        let spawned = embedded_driver_path().and_then(|p| {
            Command::new(oracle::oracle_python())
                .arg("-I")
                .arg(&p)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
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

    /// Issue one request line, read one response line. None on any I/O failure
    /// (the process is then marked dead so callers fall back gracefully — the
    /// individual call sites preserve the old per-helper degradation behavior).
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
}

impl Drop for StartupDriver {
    fn drop(&mut self) {
        self.proc = None; // closes stdin -> child exits
        if let Some(mut c) = self.child.take() {
            let _ = c.wait();
        }
    }
}

/// Process-global singleton. Lazily spawned on first use; kept alive for the
/// whole process so config discovery (start of main) and the stats save (end
/// of run) share ONE interpreter.
fn driver() -> &'static Mutex<StartupDriver> {
    static DRIVER: OnceLock<Mutex<StartupDriver>> = OnceLock::new();
    DRIVER.get_or_init(|| Mutex::new(StartupDriver::new()))
}

/// Run a closure with the global driver locked.
pub fn with_driver<R>(f: impl FnOnce(&mut StartupDriver) -> R) -> R {
    let mut g = driver().lock().expect("startup driver mutex poisoned");
    f(&mut g)
}

/// A request against the global driver (convenience).
pub fn request(req: serde_json::Value) -> Option<serde_json::Value> {
    with_driver(|d| d.request(req))
}

/// Run the interpreter sys.path probe on the unified driver and stash the raw
/// JSON in PRYLINT_PYENV_JSON for pyinfer's `pyenv::probe()` to consume (saving
/// it a redundant `python -c` spawn). Called from main() BEFORE init-hook
/// execution so the captured sys.path is pristine. On driver failure this is a
/// no-op — pyenv::probe() then falls back to self-spawning, preserving the old
/// behavior.
pub fn probe_into_env() {
    if std::env::var_os("PRYLINT_PYENV_JSON").is_some() {
        return; // already set (e.g. by a wrapper) — respect it
    }
    if let Some(v) = request(serde_json::json!({"op": "probe"})) {
        if let Ok(s) = serde_json::to_string(&v) {
            std::env::set_var("PRYLINT_PYENV_JSON", s);
        }
    }
}
