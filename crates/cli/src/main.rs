mod discover;
mod msgstate;
mod oracle;
mod reporter;
mod run;

use std::process::ExitCode;

// The phase-2 engine is allocation-heavy (Rc values, per-inference Vecs):
// samply showed ~35% self time in libsystem_malloc + 12% memset/memmove on
// django. mimalloc is a drop-in allocator swap — timing-only, no semantic
// surface (no iteration orders, no cache keys depend on addresses... except
// none do: identity keys use Rc pointers whose VALUES change run-to-run
// already under ASLR; orders are insertion-derived).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut paths: Vec<String> = Vec::new();
    let mut disables: Vec<String> = Vec::new();
    let mut errors_only = false;
    let mut jobs: Option<usize> = None;
    let mut dump_fileitems = false;
    let mut dump_ast = false;
    let mut dump_infer: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let take_value = |i: &mut usize| -> Option<String> {
            *i += 1;
            args.get(*i).cloned()
        };
        match a.as_str() {
            "-E" | "--errors-only" => errors_only = true,
            "--dump-fileitems" => dump_fileitems = true,
            "--dump-ast" => dump_ast = true,
            "--dump-infer" => {
                dump_infer = take_value(&mut i);
            }
            _ if a.starts_with("--disable=") => {
                // _csv_transformer: comma-split, whitespace-stripped items
                disables.extend(
                    a["--disable=".len()..]
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty()),
                );
            }
            "--disable" => {
                if let Some(v) = take_value(&mut i) {
                    disables.extend(
                        v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
                    );
                }
            }
            _ if a.starts_with("--rcfile=") => {} // accepted, contents ignored for now
            "--rcfile" => {
                let _ = take_value(&mut i);
            }
            _ if a.starts_with("--jobs=") => jobs = a["--jobs=".len()..].parse().ok(),
            "--jobs" | "-j" => {
                jobs = take_value(&mut i).and_then(|v| v.parse().ok());
            }
            // Output-neutral pylint options: accepted and ignored.
            // --persistent only affects pylint's stats cache (score history);
            // it changes no messages and no exit codes.
            _ if a.starts_with("--persistent=") => {}
            "--persistent" => {
                let _ = take_value(&mut i);
            }
            _ if a.starts_with('-') && a.len() > 1 => {
                // argparse error path: usage message to stderr, exit 32
                eprintln!("usage: pylint [options]");
                eprintln!("pylint: error: Unrecognized option found: {a}");
                return ExitCode::from(32);
            }
            _ => paths.push(a.clone()),
        }
        i += 1;
    }

    // prylint implements pylint's errors-only contract. Running without -E
    // would silently under-report (W/R/C checkers are not implemented), which
    // is dangerous in CI: a hook would pass on code real pylint fails. Refuse
    // loudly unless explicitly overridden.
    if !errors_only && !dump_fileitems && !dump_ast && dump_infer.is_none() {
        if std::env::var("PRYLINT_ALLOW_PARTIAL").is_err() {
            eprintln!(
                "prylint: this build implements pylint's errors-only mode (-E) with \
                 byte-identical output; running without -E would silently miss W/R/C \
                 messages that real pylint reports.\n\
                 Add -E to the invocation, or set PRYLINT_ALLOW_PARTIAL=1 to run anyway."
            );
            return ExitCode::from(32);
        }
        eprintln!(
            "prylint: warning: running without -E; W/R/C messages real pylint would \
             report are NOT checked (PRYLINT_ALLOW_PARTIAL is set)."
        );
    }

    if let Some(n) = jobs {
        if n > 0 {
            let _ = rayon::ThreadPoolBuilder::new().num_threads(n).build_global();
        }
    }

    if dump_ast {
        for path in &paths {
            println!("=== {path}");
            // astroid modutils.get_source_file: given X.pyi, prefer the
            // sibling X.py source when it exists (PY_SOURCE_EXTS order).
            let mut read_path = path.clone();
            if let Some(base) = path.strip_suffix(".pyi") {
                let py = format!("{base}.py");
                if std::path::Path::new(&py).exists() {
                    read_path = py;
                }
            }
            let bytes = match std::fs::read(&read_path) {
                Ok(b) => b,
                Err(_) => {
                    // astroid: OSError in open_source_file -> AstroidBuildingError
                    // (builder.py:120); the harness dumper prints BUILDERROR <type>.
                    println!("BUILDERROR AstroidBuildingError");
                    continue;
                }
            };
            // Error messages embed os.path.abspath of the source path
            // (modutils.get_source_file absolutizes before file_build).
            let abs = std::path::absolute(&read_path)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| read_path.clone());
            let src = match pyast::decode_source(&bytes, &abs) {
                Ok(src) => src,
                Err(pyast::DecodeError::Syntax(msg)) => {
                    // SyntaxError from detect_encoding: lineno/offset are None.
                    println!("SYNTAXERROR None:None {msg}");
                    continue;
                }
                Err(pyast::DecodeError::Lookup(msg)) => {
                    // LookupError has no lineno/offset attrs; dumper getattr -> 0.
                    println!("SYNTAXERROR 0:0 {msg}");
                    continue;
                }
                Err(pyast::DecodeError::Unicode) => {
                    println!("BUILDERROR AstroidBuildingError");
                    continue;
                }
            };
            let stem = std::path::Path::new(path)
                .file_stem()
                .map(|s| s == "__init__")
                .unwrap_or(false);
            match pyast::parse::parse_module(&src, "mod", path, stem) {
                pyast::parse::ParseOutcome { tree: Some(tree), .. } => {
                    print!("{}", tree.dump());
                }
                pyast::parse::ParseOutcome { error: Some(e), .. } => {
                    println!("SYNTAXERROR {}:{} {}", e.line, e.offset, e.message);
                }
                _ => unreachable!(),
            }
        }
        return ExitCode::SUCCESS;
    }

    if let Some(items) = dump_infer {
        let code = pyinfer::dump::run_dump_infer(&items);
        return ExitCode::from(code as u8);
    }

    if dump_fileitems {
        let cfg = discover::DiscoverConfig::default();
        let items = discover::expand_modules_fs(&paths, &cfg);
        for it in &items {
            println!(
                "{}",
                serde_json::json!({"name": it.name, "path": it.filepath})
            );
        }
        return ExitCode::SUCCESS;
    }

    if paths.is_empty() {
        // run.py:202-211: printed to STDOUT, exit 32
        println!("No files to lint: exiting.");
        return ExitCode::from(32);
    }

    let opts = run::RunOpts { paths, disables, errors_only };
    let code = run::run(&opts);
    ExitCode::from((code & 0xff) as u8)
}
