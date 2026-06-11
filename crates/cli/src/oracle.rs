//! Batched syntax-oracle subprocess: files our parser (ruff @ target 3.12)
//! rejects are re-judged by the pinned CPython/astroid via
//! harness/syntax_oracle.py, replicating pylint's get_ast() error taxonomy
//! exactly (E0001 / F0010 / F0002 / tokenize-form E0001).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub enum Verdict {
    /// astroid parses fine; optional tokenize.TokenError details
    /// (pylint tokenizes before any checker runs: pylinter.py:1079-1090)
    Ok { tokenize: Option<TokenizeErr> },
    SyntaxError { line: i64, offset: Option<i64>, msg: String },
    ParseError { msg: String },
    AstroidError,
}

#[derive(Debug, Clone)]
pub struct TokenizeErr {
    pub line: i64,
    pub col: i64,
    pub msg: String,
}

fn oracle_script_path() -> PathBuf {
    if let Ok(p) = std::env::var("PRYLINT_ORACLE") {
        return PathBuf::from(p);
    }
    // exe at <root>/target/<profile>/prylint -> <root>/harness/syntax_oracle.py
    if let Ok(exe) = std::env::current_exe() {
        if let Some(root) = exe.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
            let candidate = root.join("harness/syntax_oracle.py");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("harness/syntax_oracle.py")
}

/// Run the oracle once over all requests; one verdict per request, in order.
pub fn run_oracle(requests: &[(String, String)]) -> Vec<Verdict> {
    let fallback = vec![Verdict::AstroidError; requests.len()];
    if requests.is_empty() {
        return Vec::new();
    }
    let python = std::env::var("PRYLINT_PYTHON").unwrap_or_else(|_| "python3".to_string());
    let script = oracle_script_path();
    let mut child = match Command::new(&python)
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("prylint: failed to spawn syntax oracle {python} {script:?}: {e}");
            return fallback;
        }
    };
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    // reader thread avoids pipe deadlock for large batches
    let n = requests.len();
    let reader = std::thread::spawn(move || {
        let mut verdicts = Vec::with_capacity(n);
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            verdicts.push(parse_verdict(&line));
            if verdicts.len() == n {
                break;
            }
        }
        verdicts
    });
    for (path, modname) in requests {
        let req = serde_json::json!({"path": path, "modname": modname});
        if writeln!(stdin, "{req}").is_err() {
            break;
        }
    }
    drop(stdin);
    let mut verdicts = reader.join().unwrap_or_default();
    let _ = child.wait();
    if verdicts.len() < requests.len() {
        eprintln!(
            "prylint: syntax oracle returned {} of {} verdicts",
            verdicts.len(),
            requests.len()
        );
        verdicts.resize(requests.len(), Verdict::AstroidError);
    }
    verdicts
}

/// Persistent oracle coprocess for the imports checker: per-(path, resolved
/// modname) exact `str(AstroidSyntaxError.error)` strings. The oracle is
/// invoked with the RESOLVED module name so the SyntaxError filename token
/// embeds exactly what astroid's import-time build would produce
/// (imports.py:1032-1036 `Cannot import {modname!r} due to '{exc.error}'`).
pub struct OracleProc {
    proc: Option<(std::process::ChildStdin, BufReader<std::process::ChildStdout>)>,
    child: Option<std::process::Child>,
    spawn_failed: bool,
    cache: rustc_hash::FxHashMap<(String, String), Option<String>>,
}

impl Default for OracleProc {
    fn default() -> Self {
        OracleProc { proc: None, child: None, spawn_failed: false, cache: Default::default() }
    }
}

impl OracleProc {
    /// Spawn the coprocess eagerly so the python interpreter boots while the
    /// caller does other work; the first `query` then finds it ready instead
    /// of paying the ~80ms interpreter start inline. Behavior is otherwise
    /// identical to the lazy `ensure` on first query.
    pub fn prespawn(&mut self) {
        self.ensure();
    }

    fn ensure(&mut self) -> bool {
        if self.proc.is_some() {
            return true;
        }
        if self.spawn_failed {
            return false;
        }
        let python = std::env::var("PRYLINT_PYTHON").unwrap_or_else(|_| "python3".to_string());
        let script = oracle_script_path();
        match Command::new(&python)
            .arg(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(mut child) => {
                let stdin = child.stdin.take().expect("piped stdin");
                let stdout = child.stdout.take().expect("piped stdout");
                self.proc = Some((stdin, BufReader::new(stdout)));
                self.child = Some(child);
                true
            }
            Err(e) => {
                eprintln!("prylint: failed to spawn syntax oracle {python} {script:?}: {e}");
                self.spawn_failed = true;
                false
            }
        }
    }

    /// `str(exc.error)` for an AstroidSyntaxError raised when building
    /// (path, modname); None when astroid builds it fine or fails with a
    /// non-syntax error.
    pub fn syntax_error_str(&mut self, path: &str, modname: &str) -> Option<String> {
        let key = (path.to_string(), modname.to_string());
        if let Some(v) = self.cache.get(&key) {
            return v.clone();
        }
        let res = self.query(path, modname);
        self.cache.insert(key, res.clone());
        res
    }

    fn query(&mut self, path: &str, modname: &str) -> Option<String> {
        if !self.ensure() {
            return None;
        }
        let (stdin, stdout) = self.proc.as_mut().unwrap();
        let req = serde_json::json!({"path": path, "modname": modname});
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
        match parse_verdict(line.trim()) {
            Verdict::SyntaxError { msg, .. } => {
                // msg = "Parsing failed: '<str(exc.error)>'"
                let inner = msg
                    .strip_prefix("Parsing failed: '")
                    .and_then(|s| s.strip_suffix('\''))
                    .unwrap_or(&msg);
                Some(inner.to_string())
            }
            _ => None,
        }
    }
}

impl Drop for OracleProc {
    fn drop(&mut self) {
        self.proc = None; // closes stdin -> child exits
        if let Some(mut c) = self.child.take() {
            let _ = c.wait();
        }
    }
}

fn parse_verdict(line: &str) -> Verdict {
    let v: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Verdict::AstroidError,
    };
    if v.get("ok").and_then(|b| b.as_bool()) == Some(true) {
        let tokenize = v.get("tokenize").and_then(|t| {
            Some(TokenizeErr {
                line: t.get("line")?.as_i64()?,
                col: t.get("col")?.as_i64()?,
                msg: t.get("msg")?.as_str()?.to_string(),
            })
        });
        return Verdict::Ok { tokenize };
    }
    match v.get("kind").and_then(|k| k.as_str()) {
        Some("syntax-error") => Verdict::SyntaxError {
            line: v.get("line").and_then(|l| l.as_i64()).unwrap_or(0),
            offset: v.get("offset").and_then(|o| o.as_i64()),
            msg: v.get("msg").and_then(|m| m.as_str()).unwrap_or("").to_string(),
        },
        Some("parse-error") => Verdict::ParseError {
            msg: v.get("msg").and_then(|m| m.as_str()).unwrap_or("").to_string(),
        },
        _ => Verdict::AstroidError,
    }
}
