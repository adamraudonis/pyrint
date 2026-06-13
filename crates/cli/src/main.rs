mod config;
mod discover;
mod msgstate;
mod oracle;
mod reporter;
mod run;
mod stats;

use std::process::ExitCode;

// The phase-2 engine is allocation-heavy (Rc values, per-inference Vecs):
// samply showed ~35% self time in libsystem_malloc + 12% memset/memmove on
// django. mimalloc is a drop-in allocator swap — timing-only, no semantic
// surface (no iteration orders, no cache keys depend on addresses... except
// none do: identity keys use Rc pointers whose VALUES change run-to-run
// already under ASLR; orders are insertion-derived).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// _yn_validator (argument_parsing): accepts y/yes/true and n/no/false
/// (case-insensitive). Anything else is an argparse error -> exit 32.
fn parse_yn(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "y" | "yes" | "true" => Some(true),
        "n" | "no" | "false" => Some(false),
        _ => None,
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut paths: Vec<String> = Vec::new();
    let mut disables: Vec<String> = Vec::new();
    let mut enables: Vec<(run::EnableSrc, String)> = Vec::new();
    let mut errors_only = false;
    let mut jobs: Option<i64> = None;
    let mut dump_fileitems = false;
    let mut dump_ast = false;
    let mut dump_infer: Option<String> = None;
    let mut score = true;
    let mut persistent = true;
    let mut fail_under: f64 = 10.0;
    let mut fail_on: Vec<String> = Vec::new();
    let mut exit_zero = false;
    let mut rcfile: Option<String> = None;
    let mut init_hooks: Vec<String> = Vec::new();
    let mut cli_set_score = false;
    let mut cli_set_persistent = false;
    let mut cli_set_fail_under = false;
    let mut i = 0;
    // helper to split a csv option value (whitespace-stripped, non-empty)
    let csv = |v: &str| -> Vec<String> {
        v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
    };
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
                disables.extend(csv(&a["--disable=".len()..]));
            }
            "--disable" | "-d" => {
                if let Some(v) = take_value(&mut i) {
                    disables.extend(csv(&v));
                }
            }
            _ if a.starts_with("--enable=") => {
                enables.extend(csv(&a["--enable=".len()..]).into_iter().map(|s| (run::EnableSrc::Cli, s)));
            }
            "--enable" | "-e" => {
                if let Some(v) = take_value(&mut i) {
                    enables.extend(csv(&v).into_iter().map(|s| (run::EnableSrc::Cli, s)));
                }
            }
            _ if a.starts_with("--rcfile=") => rcfile = Some(a["--rcfile=".len()..].to_string()),
            "--rcfile" => rcfile = take_value(&mut i),
            _ if a.starts_with("--init-hook=") => init_hooks.push(a["--init-hook=".len()..].to_string()),
            "--init-hook" => {
                if let Some(v) = take_value(&mut i) {
                    init_hooks.push(v);
                }
            }
            _ if a.starts_with("--jobs=") => jobs = a["--jobs=".len()..].parse().ok(),
            "--jobs" | "-j" => {
                jobs = take_value(&mut i).and_then(|v| v.parse().ok());
            }
            // --score (-s): yn. Gates the footer display only.
            _ if a.starts_with("--score=") => match parse_yn(&a["--score=".len()..]) {
                Some(b) => {
                    score = b;
                    cli_set_score = true;
                }
                None => return yn_error(a),
            },
            "--score" | "-s" => match take_value(&mut i).as_deref().and_then(parse_yn) {
                Some(b) => {
                    score = b;
                    cli_set_score = true;
                }
                None => return yn_error(a),
            },
            // --persistent: yn. Gates writing the stats pickle.
            _ if a.starts_with("--persistent=") => match parse_yn(&a["--persistent=".len()..]) {
                Some(b) => {
                    persistent = b;
                    cli_set_persistent = true;
                }
                None => return yn_error(a),
            },
            "--persistent" => match take_value(&mut i).as_deref().and_then(parse_yn) {
                Some(b) => {
                    persistent = b;
                    cli_set_persistent = true;
                }
                None => return yn_error(a),
            },
            _ if a.starts_with("--fail-under=") => {
                fail_under = a["--fail-under=".len()..].parse().unwrap_or(10.0);
                cli_set_fail_under = true;
            }
            "--fail-under" => {
                if let Some(v) = take_value(&mut i) {
                    fail_under = v.parse().unwrap_or(10.0);
                    cli_set_fail_under = true;
                }
            }
            _ if a.starts_with("--fail-on=") => fail_on.extend(csv(&a["--fail-on=".len()..])),
            "--fail-on" => {
                if let Some(v) = take_value(&mut i) {
                    fail_on.extend(csv(&v));
                }
            }
            "--exit-zero" => exit_zero = true,
            // Output-neutral pylint options accepted and ignored (no message
            // or exit effect): output-format text is the only supported one,
            // reports default off, msg-template not supported.
            _ if a.starts_with("--output-format=") || a == "--output-format" => {
                if a == "--output-format" {
                    let _ = take_value(&mut i);
                }
            }
            _ if a.starts_with("--reports=") || a == "-r" || a == "--reports" => {
                if a == "--reports" {
                    let _ = take_value(&mut i);
                }
            }
            "--verbose" | "-v" => {} // verbose footer line not a parity target
            _ if a.starts_with('-') && a.len() > 1 => {
                eprintln!("usage: pylint [options]");
                eprintln!("pylint: error: Unrecognized option found: {a}");
                return ExitCode::from(32);
            }
            _ => paths.push(a.clone()),
        }
        i += 1;
    }

    // Config-file discovery + parsing (rcfile if given, else the default-config
    // search order). enable/disable accumulate in parse order: file values are
    // applied BEFORE the CLI values, so file disables are PREPENDED to the CLI
    // disable list (notes/09 §8.6). For ordinary store-options the CLI wins
    // over the file, which wins over the built-in default.
    {
        let cfg = config::load_config(rcfile.as_deref(), &mut init_hooks);
        if let Some(c) = cfg {
            let mut merged_disable = c.disable;
            merged_disable.extend(std::mem::take(&mut disables));
            disables = merged_disable;
            let mut merged_enable: Vec<(run::EnableSrc, String)> =
                c.enable.into_iter().map(|s| (run::EnableSrc::Config, s)).collect();
            merged_enable.extend(std::mem::take(&mut enables));
            enables = merged_enable;
            // store-options: file value applies only where the CLI left the
            // default (CLI precedence). cli_set_* track explicit CLI presence.
            if !cli_set_score {
                if let Some(b) = c.score {
                    score = b;
                }
            }
            if !cli_set_persistent {
                if let Some(b) = c.persistent {
                    persistent = b;
                }
            }
            if !cli_set_fail_under {
                if let Some(f) = c.fail_under {
                    fail_under = f;
                }
            }
            if fail_on.is_empty() {
                fail_on = c.fail_on;
            }
        }
    }
    // -E still forces score/persistent off even if a config file set them.
    if errors_only {
        score = false;
        persistent = false;
    }

    // init-hook execution: run the hooks through python and capture sys.path
    // additions so import resolution sees them. Non-path effects can't be
    // replicated in-process; warn loudly on stderr.
    if !init_hooks.is_empty() {
        config::run_init_hooks(&init_hooks);
    }

    if let Some(n) = jobs {
        // run.py:213-218: jobs < 0 -> stderr + exit 32.
        if n < 0 {
            eprintln!("Jobs number ({n}) should be greater than or equal to 0");
            return ExitCode::from(32);
        }
    }
    let jobs: Option<usize> = jobs.map(|n| n as usize);
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

    let opts = run::RunOpts {
        paths,
        disables,
        enables,
        errors_only,
        score,
        persistent,
        fail_under,
        fail_on,
        exit_zero,
    };
    let code = run::run(&opts);
    ExitCode::from((code & 0xff) as u8)
}

/// argparse yn-validation failure -> usage + error on stderr, exit 32.
fn yn_error(opt: &str) -> ExitCode {
    let name = opt.split('=').next().unwrap_or(opt);
    eprintln!("usage: pylint [options]");
    eprintln!("pylint: error: argument {name}: invalid yn value");
    ExitCode::from(32)
}
