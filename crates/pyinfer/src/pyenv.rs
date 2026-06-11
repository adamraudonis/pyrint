//! Pinned-interpreter environment probe. The module graph resolves imports
//! over `[realpath(corpus root)] + <venv sys.path>` exactly like the oracle
//! (harness/dump_infer.py inserts realpath(root) at sys.path[0]) and real
//! pylint (augmented_sys_path). We obtain the venv's sys.path & friends by
//! running PRYLINT_PYTHON once and cache the result.

use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct PyEnv {
    /// venv sys.path entries (without the corpus root)
    pub sys_path: Vec<String>,
    pub builtin_module_names: Vec<String>,
    pub stdlib_module_names: Vec<String>,
    /// importlib.machinery.EXTENSION_SUFFIXES
    pub ext_suffixes: Vec<String>,
    /// os.path.__file__ (modutils.file_info_from_modpath special case)
    pub os_path_file: String,
    /// frozen-module spec table (spec.py:169-192): full dotted name ->
    /// loader_state.filename (None for sourceless e.g. _frozen_importlib).
    /// Only names whose importlib.util.find_spec loader is FrozenImporter.
    pub frozen_specs: std::collections::HashMap<String, Option<String>>,
}

fn python_exe() -> String {
    if let Ok(p) = std::env::var("PRYLINT_PYTHON") {
        return p;
    }
    // dev convenience: <repo>/target/release/prylint -> <repo>/.venv-pylint
    if let Ok(exe) = std::env::current_exe() {
        let mut p: PathBuf = exe;
        for _ in 0..3 {
            p = match p.parent() {
                Some(parent) => parent.to_path_buf(),
                None => break,
            };
        }
        let cand = p.join(".venv-pylint/bin/python");
        if cand.exists() {
            return cand.to_string_lossy().into_owned();
        }
    }
    "python3".to_string()
}

const PROBE: &str = r#"
import importlib.machinery, importlib.util, json, os, sys, _imp
frozen = {}
for name in _imp._frozen_module_names():
    try:
        spec = importlib.util.find_spec(name)
    except (ValueError, ImportError):
        continue
    if spec and spec.loader is importlib.machinery.FrozenImporter:
        frozen[name] = getattr(spec.loader_state, "filename", None)
print(json.dumps({
    "sys_path": sys.path,
    "builtin": list(sys.builtin_module_names),
    "stdlib": list(sys.stdlib_module_names),
    "ext_suffixes": importlib.machinery.EXTENSION_SUFFIXES,
    "os_path_file": os.path.__file__,
    "frozen_specs": frozen,
}))
"#;

pub fn probe() -> PyEnv {
    let exe = python_exe();
    let out = Command::new(&exe).arg("-c").arg(PROBE).output();
    let parsed: Option<serde_json::Value> = out
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| serde_json::from_slice(&o.stdout).ok());
    match parsed {
        Some(v) => {
            let arr = |key: &str| -> Vec<String> {
                v[key]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let frozen_specs = v["frozen_specs"]
                .as_object()
                .map(|o| {
                    o.iter()
                        .map(|(k, val)| (k.clone(), val.as_str().map(|s| s.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            PyEnv {
                sys_path: arr("sys_path"),
                builtin_module_names: arr("builtin"),
                stdlib_module_names: arr("stdlib"),
                ext_suffixes: arr("ext_suffixes"),
                os_path_file: v["os_path_file"].as_str().unwrap_or("").to_string(),
                frozen_specs,
            }
        }
        None => {
            eprintln!(
                "prylint: cannot probe the python environment ({exe} failed to run); \
                 install python3 or set PRYLINT_PYTHON. Module resolution is limited \
                 to the project root (stdlib/site-packages imports unresolved)."
            );
            PyEnv {
                sys_path: Vec::new(),
                builtin_module_names: Vec::new(),
                stdlib_module_names: Vec::new(),
                ext_suffixes: vec![".so".into()],
                os_path_file: String::new(),
                frozen_specs: Default::default(),
            }
        }
    }
}
