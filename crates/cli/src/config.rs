//! Config-file discovery + parsing and init-hook execution (Phase F,
//! notes/09-pipeline-noE.md §8). The actual INI/TOML parsing is delegated to
//! the embedded stdlib-only `config_helper.py` (python3 -I) so configparser /
//! tomllib edge semantics match pylint bug-for-bug without a Rust parser.
//!
//! - `load_config(rcfile, init_hooks)`: resolve the config path (explicit
//!   --rcfile, else find_default_config_files' first yield), parse it, and
//!   return the option values prylint consumes. init-hook values are appended
//!   to `init_hooks` (executed by the caller before linting).
//! - `run_init_hooks`: exec the hooks under python, forwarding sys.path
//!   additions to the inference engine via PRYLINT_EXTRA_SYSPATH; non-path
//!   side effects warn loudly on stderr (can't be replicated in-process).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::oracle;

const HELPER_SRC: &str = include_str!("config_helper.py");

/// Config-file options prylint consumes (the subset that changes output / exit
/// in full mode). Each is None/empty when the file did not set it.
#[derive(Default)]
pub struct FileConfig {
    pub disable: Vec<String>,
    pub enable: Vec<String>,
    pub score: Option<bool>,
    pub persistent: Option<bool>,
    pub fail_under: Option<f64>,
    pub fail_on: Vec<String>,
}

fn embedded_helper_path() -> std::io::Result<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    HELPER_SRC.hash(&mut h);
    let dir = std::env::temp_dir();
    let path = dir.join(format!("prylint-config-helper-{:016x}.py", h.finish()));
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if existing == HELPER_SRC {
            return Ok(path);
        }
    }
    let tmp =
        dir.join(format!("prylint-config-helper-{:016x}.{}.tmp", h.finish(), std::process::id()));
    std::fs::write(&tmp, HELPER_SRC)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

fn helper_request(req: serde_json::Value) -> Option<serde_json::Value> {
    let helper = embedded_helper_path().ok()?;
    let mut child = Command::new(oracle::oracle_python())
        .arg("-I")
        .arg(&helper)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .ok()?;
    {
        let mut stdin = child.stdin.take()?;
        writeln!(stdin, "{req}").ok()?;
    } // drop stdin -> EOF
    let stdout = child.stdout.take()?;
    let mut line = String::new();
    let mut reader = BufReader::new(stdout);
    let _ = reader.read_line(&mut line);
    let _ = child.wait();
    serde_json::from_str(line.trim()).ok()
}

/// Resolve and parse the active config file. Returns None when no config file
/// is found (or python is unavailable). init-hook values are appended to
/// `init_hooks` (config-file init-hooks run first).
pub fn load_config(rcfile: Option<&str>, init_hooks: &mut Vec<String>) -> Option<FileConfig> {
    // 1. determine the path
    let path: String = match rcfile {
        Some(p) => p.to_string(),
        None => {
            let cwd = std::env::current_dir().ok()?.to_string_lossy().into_owned();
            let resp = helper_request(serde_json::json!({"op":"discover","cwd": cwd}))?;
            match resp.get("path").and_then(|v| v.as_str()) {
                Some(p) => p.to_string(),
                None => return None, // no default config file
            }
        }
    };

    // 2. parse it
    let resp = helper_request(serde_json::json!({"op":"parse","path": path}))?;

    // A FileNotFoundError on an explicit --rcfile is the exit-32 path
    // (config_initialization.py:45-50). A configparser/TOML error is F0011 —
    // we surface it on stderr and continue without config (best-effort; the
    // profiles use a valid empty rcfile so neither fires).
    if let Some(err) = resp.get("err").and_then(|v| v.as_str()) {
        let kind = resp.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        if rcfile.is_some() && kind == "FileNotFoundError" {
            eprintln!("Unable to read the config file {path}: {err}");
            std::process::exit(32);
        }
        eprintln!("prylint: config-parse-error in {path}: {err}");
        return None;
    }

    // 3. collect init-hooks (config-file ones run first)
    if let Some(hooks) = resp.get("init_hooks").and_then(|v| v.as_array()) {
        let mut collected: Vec<String> = hooks
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        collected.extend(std::mem::take(init_hooks));
        *init_hooks = collected;
    }

    // 4. map option values
    let options = resp.get("options").and_then(|v| v.as_object())?;
    let mut fc = FileConfig::default();
    let csv_all = |key: &str| -> Vec<String> {
        options
            .get(key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .flat_map(|s| s.split(',').map(|x| x.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };
    let last_str = |key: &str| -> Option<String> {
        options
            .get(key)
            .and_then(|v| v.as_array())
            .and_then(|a| a.last())
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    fc.disable = csv_all("disable");
    fc.enable = csv_all("enable");
    fc.fail_on = csv_all("fail-on");
    fc.score = last_str("score").and_then(|s| parse_yn(&s));
    fc.persistent = last_str("persistent").and_then(|s| parse_yn(&s));
    fc.fail_under = last_str("fail-under").and_then(|s| s.parse().ok());
    Some(fc)
}

fn parse_yn(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "y" | "yes" | "true" => Some(true),
        "n" | "no" | "false" => Some(false),
        _ => None,
    }
}

/// Execute the init-hooks under python and forward sys.path additions to the
/// inference engine via PRYLINT_EXTRA_SYSPATH. The hook is `exec`'d exactly as
/// pylint's _preprocess_options does (config/utils.py); we capture the
/// resulting sys.path delta (entries not in the baseline interpreter sys.path)
/// so import resolution sees them. Other side effects (env vars, monkeypatches)
/// cannot be replicated in our Rust process -> a single loud stderr warning.
pub fn run_init_hooks(hooks: &[String]) {
    let py = oracle::oracle_python();
    // Build a small driver: snapshot sys.path, exec each hook, print the new
    // entries (in order) one per line.
    let joined = hooks.join("\n");
    let driver = format!(
        "import sys, json\n_before=list(sys.path)\n_hooks={hooks_json}\nfor _h in _hooks:\n    exec(_h)\n_after=sys.path\n_added=[p for p in _after if p not in _before]\nsys.stderr.write('')\nprint('\\x00'.join(_added))\n",
        hooks_json = serde_json::to_string(hooks).unwrap_or_else(|_| "[]".to_string())
    );
    let _ = joined; // documented intent
    match Command::new(&py).arg("-c").arg(&driver).stderr(Stdio::inherit()).output() {
        Ok(out) if out.status.success() => {
            let added = String::from_utf8_lossy(&out.stdout);
            let entries: Vec<&str> =
                added.trim().split('\u{0}').filter(|s| !s.is_empty()).collect();
            if !entries.is_empty() {
                // forward to the engine; absolutize relative entries against cwd
                let abs: Vec<String> = entries
                    .iter()
                    .map(|e| {
                        std::path::absolute(e)
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_else(|_| e.to_string())
                    })
                    .collect();
                std::env::set_var("PRYLINT_EXTRA_SYSPATH", abs.join(":"));
            }
            eprintln!(
                "prylint: ran {} init-hook(s); forwarded {} sys.path addition(s). \
                 NOTE: any non-sys.path side effects of init-hook are NOT replicated \
                 (prylint runs hooks in a probe subprocess, not in-process).",
                hooks.len(),
                entries.len()
            );
        }
        Ok(out) => {
            eprintln!(
                "prylint: init-hook execution failed (exit {}); continuing without its effects.",
                out.status.code().unwrap_or(-1)
            );
        }
        Err(e) => {
            eprintln!("prylint: cannot run init-hook ({py}: {e}); continuing.");
        }
    }
}
