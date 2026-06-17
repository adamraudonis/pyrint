//! Config-file discovery + parsing and init-hook execution (Phase F,
//! notes/09-pipeline-noE.md §8). The actual INI/TOML parsing is delegated to
//! the unified stdlib-only startup driver (`startup_driver.py`, run as ONE
//! persistent `python -I` coprocess — see startup.rs) so configparser /
//! tomllib edge semantics match pylint bug-for-bug without a Rust parser, and
//! discover+parse+probe+init-hook+stats all amortize over a single interpreter
//! boot (the old code spawned config-helper TWICE: discover then parse).
//!
//! - `load_config(rcfile, init_hooks)`: resolve the config path (explicit
//!   --rcfile, else find_default_config_files' first yield), parse it, and
//!   return the option values prylint consumes. init-hook values are appended
//!   to `init_hooks` (executed by the caller before linting).
//! - `run_init_hooks`: exec the hooks under python, forwarding sys.path
//!   additions to the inference engine via PRYLINT_EXTRA_SYSPATH; non-path
//!   side effects warn loudly on stderr (can't be replicated in-process).

use crate::startup;

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

/// Issue one request against the persistent unified startup driver (ONE python
/// process for discover + parse + probe + init-hook + stats — see startup.rs).
/// Replaces the former per-call config-helper spawn, which spawned twice
/// (discover then parse).
fn helper_request(req: serde_json::Value) -> Option<serde_json::Value> {
    startup::request(req)
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
    // Execute the hooks inside the SAME persistent startup interpreter that did
    // config discovery (startup_driver.py "inithook" op): it snapshots sys.path,
    // exec's each hook in order, and returns the entries the hooks added. This
    // is the same exec semantics as before (a dedicated `python -c` subprocess);
    // it now reuses the already-running driver instead of spawning again.
    let resp = startup::request(serde_json::json!({"op":"inithook","hooks": hooks}));
    match resp {
        Some(v) if v.get("ok").and_then(|b| b.as_bool()) == Some(true) => {
            let entries: Vec<String> = v
                .get("added")
                .and_then(|a| a.as_array())
                .map(|arr| arr.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
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
        _ => {
            eprintln!(
                "prylint: init-hook execution failed; continuing without its effects."
            );
        }
    }
}
