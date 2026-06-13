//! Lint pipeline orchestration: the Rust counterpart of PyLinter.check
//! (pylinter.py:672-727) for the `prylint . -E --disable=...` invocation.
//!
//! Phase 1 (`_get_asts`, pylinter.py:729-759): parse ALL files (rayon
//! parallel, results in file order); parse-phase messages (E0001/F0010/F0002)
//! print before any check-phase message, in file order. Ruff-rejected files
//! are re-judged by a single batched syntax-oracle subprocess.
//!
//! Phase 2 (`_lint_files`, pylinter.py:771-830): per file in order:
//! tokenize-form E0001 -> pragmas -> skip-file -> raw checkers (unicode) ->
//! AST walk (placeholder; statement counting only). Parallelized with rayon,
//! messages buffered per file and flushed strictly in file order.

use std::io::BufWriter;

use rayon::prelude::*;

use pyast::parse::TokEvent;
use pyast::source::{decode_source, DecodeError, SourceFile};
use pyast::tree::{NodeId, NodeKind, Tree};
use pycheckers::msgstore::{self, store, ResolveError};

use crate::discover::{self, FileItem};
use crate::msgstate::{is_message_enabled, process_tokens, FileState, GlobalState};
use crate::oracle::{self, Verdict};
use crate::reporter::{OutMsg, Reporter};

pub struct RunOpts {
    pub paths: Vec<String>,
    /// --disable values, comma-split, in CLI order
    pub disables: Vec<String>,
    /// -E/--errors-only. With -E the baked global state (msgs.rs `enabled`)
    /// applies; without it the full-pylint default state (377 enabled
    /// messages) is seeded and the token/raw W/C checkers run.
    pub errors_only: bool,
}

struct ParsedFile {
    tree: Tree,
    src: SourceFile,
    tokens: Vec<TokEvent>,
    abspath: String,
    /// unicode raw-checker results, precomputed during the parallel parse
    /// phase from the same bytes the parse read (pure function of bytes;
    /// emission order preserved). Replayed in phase 2 where the sequential
    /// checker used to run, so pragma handling is unchanged.
    unicode_msgs: Vec<pycheckers::unicode::UnicodeMsg>,
    /// FormatChecker token-phase messages (full mode; precomputed in
    /// parallel — pure function of the raw bytes EXCEPT the C0302 line
    /// attribution, which is re-resolved at flush time against the
    /// run-global _pragma_lineno map).
    fmt_msgs: Vec<pycheckers::format::FmtMsg>,
    /// EncodingChecker (fixme) token-phase messages (full mode)
    fixme_msgs: Vec<pycheckers::miscck::FixmeMsg>,
    /// RefactoringChecker token-phase scan: _elifs positions + R1707 lines
    /// (full mode). Pure function of the raw token stream; the _elifs feed
    /// the AST walk (and leak across modules when R1732 disabled).
    refac_tokens: pycheckers::refactoring::RefacTokens,
    /// SimilaritiesChecker readlines() of the file (full mode, R0801): the
    /// raw real lines (with terminators, BOM-stripped for utf-8-sig, split on
    /// \n/\r\n/\r) the LineSet stores. None when the checker isn't prepared;
    /// empty Vec on a UnicodeDecodeError (matches append_stream's bail-out).
    sim_lines: Option<Vec<String>>,
}

enum FileData {
    Parsed(Box<ParsedFile>),
    /// phase-1 message: (msgid, line-as-displayed, col, text)
    Phase1 { msgid: &'static str, line: i64, col: i64, text: String },
    /// ruff rejected; awaiting oracle verdict
    NeedsOracle,
}

#[derive(Default, Clone, Copy)]
struct Stats {
    fatal: u64,
    error: u64,
    warning: u64,
    refactor: u64,
    convention: u64,
    info: u64,
    statements: u64,
}

impl Stats {
    fn count(&mut self, msgid: &str) {
        match msgid.chars().next().unwrap_or('I') {
            'F' => self.fatal += 1,
            'E' => self.error += 1,
            'W' => self.warning += 1,
            'R' => self.refactor += 1,
            'C' => self.convention += 1,
            _ => self.info += 1,
        }
    }
}

struct ModuleOut {
    msgs: Vec<OutMsg>,
    stats: Stats,
    linted: bool,
}

/// Env-gated phase timing (PRYLINT_PHASE_TIMING=1): prints coarse phase
/// durations to stderr. Debug aid only; no output-visible effect.
fn phase_timing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("PRYLINT_PHASE_TIMING").is_ok())
}

macro_rules! phase_mark {
    ($t0:expr, $label:expr) => {
        if phase_timing() {
            eprintln!("[phase] {:<12} {:8.3}s", $label, $t0.elapsed().as_secs_f64());
        }
    };
}

pub fn run(opts: &RunOpts) -> i32 {
    let t0 = std::time::Instant::now();
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    // BaseReporter.path_strip_prefix = os.getcwd() + os.sep (base_reporter.py:37)
    let strip_prefix = format!("{cwd}/");

    // ---- config phase ------------------------------------------------
    let mut global = if opts.errors_only {
        GlobalState::for_target_flags()
    } else {
        GlobalState::full_default()
    };
    // unknown/deleted --disable values are stashed and printed under module
    // "Command line" BEFORE -E folds W/R away (config_initialization.py:128
    // runs before _parse_error_mode at :145 — empirically confirmed,
    // notes/02 §18.5)
    let mut stashed: Vec<(&'static str, String)> = Vec::new();
    for item in &opts.disables {
        match global.cli_disable(item) {
            Ok(()) => {}
            Err(ResolveError::Unknown) => stashed.push((
                "W0012",
                format!(
                    "Unknown option value for '--disable', expected a valid pylint message and got '{item}'"
                ),
            )),
            Err(ResolveError::Deleted(s)) | Err(ResolveError::Moved(s)) => {
                stashed.push(("R0022", format!("Useless option value for '--disable', {s}")))
            }
        }
    }

    let stdout = std::io::stdout();
    let mut reporter = Reporter::new(BufWriter::new(stdout.lock()));

    for (msgid, text) in &stashed {
        // suppressed only by an explicit --disable of the message itself
        // (the -E category disable has not run yet at this point)
        let explicit = opts.disables.iter().any(|d| {
            d == msgid
                || (*msgid == "W0012" && d == "unknown-option-value")
                || (*msgid == "R0022" && d == "useless-option-value")
        });
        if explicit {
            continue;
        }
        let idx = store().by_msgid[msgid];
        reporter.handle(&OutMsg {
            module: "Command line".to_string(),
            path: "Command line".to_string(),
            line: 1,
            col: 0,
            msgid,
            symbol: msgstore::def(idx).symbol,
            text: text.clone(),
        });
        // config-phase messages set msg_status but their stats are zeroed by
        // PyLinter.open() before checking starts (linterstats.py:328-335)
    }

    // ---- discovery ----------------------------------------------------
    let cfg = discover::DiscoverConfig::default();
    let items = discover::expand_modules_fs(&opts.paths, &cfg);

    // prepare_checkers (pylinter.py:588-598): the unicode raw checker is
    // kept iff any of its messages is enabled package-wise. Decided here
    // (config-only) so the parse phase can precompute its results.
    let unicode_prepared = ["E2501", "E2502", "C2503", "E2510", "E2511", "E2512", "E2513", "E2514", "E2515"]
        .iter()
        .any(|m| global.enabled(store().by_msgid[m]));
    // full-mode prepared/only_required gates for the new checkers (all
    // false under -E: those checkers are config-dropped there)
    let enabled_by_id = |m: &str| global.enabled(store().by_msgid[m]);
    let prep = pycheckers::walker::Prepared::from_enabled(&enabled_by_id, !opts.errors_only);
    // token-checker preparation (format / miscellaneous): -E drops both
    let format_prepared = prep.format;
    let misc_prepared = !opts.errors_only && enabled_by_id("W0511");
    // RefactoringChecker token scan (full mode): kept iff any R17xx enabled.
    let refac_prepared = prep.refac_kept;
    let refac_r1707 = prep.refac_r1707;
    // SimilaritiesChecker (R0801) prepared: default --reports=n disables
    // RP0801, so the checker is prepared iff R0801 is enabled
    // (notes/09-design-similarities §B.1). Under -E R0801 is disabled.
    let sim_prepared = !opts.errors_only && enabled_by_id("R0801");

    // ---- phase 1: parse all files (rayon, results in file order) ------
    phase_mark!(t0, "discover");
    let mut data: Vec<FileData> = items
        .par_iter()
        .map(|it| {
            parse_one(
                it,
                unicode_prepared,
                format_prepared,
                misc_prepared,
                refac_prepared,
                refac_r1707,
                sim_prepared,
            )
        })
        .collect();
    phase_mark!(t0, "parse");
    if std::env::var("PRYLINT_PARSE_ONLY").is_ok() {
        eprintln!("[phase] parse-only abort (files={})", data.len());
        std::process::exit(0);
    }

    // batched oracle pass for ruff-rejected files. The subprocess (python
    // interpreter boot + per-file verdicts) runs on its OWN thread so it
    // overlaps with the phase-2 engine below — pure scheduling: verdicts
    // only feed phase-1 messages and engine-less plans (TokenizeErr/
    // Untracked); the engine thread's input sequence is verdict-independent.
    let oracle_idx: Vec<usize> = data
        .iter()
        .enumerate()
        .filter(|(_, d)| matches!(d, FileData::NeedsOracle))
        .map(|(i, _)| i)
        .collect();
    let requests: Vec<(String, String)> = oracle_idx
        .iter()
        .map(|&i| (items[i].filepath.clone(), items[i].name.clone()))
        .collect();

    // split parse results: tree plans (engine thread) vs phase-1 messages
    let mut trees: Vec<(usize, Box<ParsedFile>)> = Vec::new();
    let mut phase1: Vec<(usize, &'static str, i64, i64, String)> = Vec::new();
    for (i, d) in data.into_iter().enumerate() {
        match d {
            FileData::Parsed(p) => trees.push((i, p)),
            FileData::Phase1 { msgid, line, col, text } => phase1.push((i, msgid, line, col, text)),
            FileData::NeedsOracle => {} // verdict handled below
        }
    }

    // ---- astroid-crash candidates: files whose tree is deep enough to
    // RecursionError astroid's rebuilder (empirical boundary ~495 nested
    // BinOps at the default recursion limit; threshold kept well below).
    // The pinned-astroid oracle delivers the EXACT pylint verdict for each
    // candidate: AstroidError -> phase-1 F0002 (pylinter._get_asts caught
    // AstroidBuildingError), never linted, and any later import of the file
    // re-crashes the importing module's check (engine crash set).
    // Verdict Ok -> linted normally (the depth probe was conservative).
    let mut crash_abs: Vec<String> = Vec::new();
    {
        let deep: Vec<usize> = trees
            .iter()
            .filter(|(_, p)| tree_depth(&p.tree) >= 350)
            .map(|(i, _)| *i)
            .collect();
        if !deep.is_empty() {
            let reqs: Vec<(String, String)> = deep
                .iter()
                .map(|&i| (items[i].filepath.clone(), items[i].name.clone()))
                .collect();
            let verdicts = oracle::run_oracle(&reqs);
            let mut dead: std::collections::HashSet<usize> = Default::default();
            for (&i, verdict) in deep.iter().zip(verdicts.into_iter()) {
                match verdict {
                    Verdict::AstroidError => {
                        phase1.push((i, "F0002", 1, 0, fatal_error_text(&items[i].filepath)));
                        if let Some((_, pf)) = trees.iter().find(|(j, _)| *j == i) {
                            crash_abs.push(pf.abspath.clone());
                        }
                        dead.insert(i);
                    }
                    Verdict::SyntaxError { line, offset, msg } => {
                        phase1.push((
                            i,
                            "E0001",
                            if line == 0 { 1 } else { line },
                            offset.unwrap_or(0),
                            msg,
                        ));
                        dead.insert(i);
                    }
                    Verdict::ParseError { msg } => {
                        phase1.push((i, "F0010", 1, 0, format!("error while code parsing: {msg}")));
                        dead.insert(i);
                    }
                    Verdict::Ok { .. } => {}
                }
            }
            if !dead.is_empty() {
                trees.retain(|(i, _)| !dead.contains(i));
            }
        }
    }
    let crash_abs = crash_abs;

    // ---- phase 2: per-module checks (sequential on a big-stack engine
    // thread: astroid's caches are process-global and order-sensitive, and
    // checker inference must run in pylint's file order) ------------------
    let mut stats = Stats::default();
    let mut extras: Vec<(usize, ModuleOut)> = Vec::new();
    let results: Vec<(usize, ModuleOut)> = std::thread::scope(|scope| {
        let items = &items;
        let trees = &trees;
        let global = &global;
        let prep = prep;
        let strip_prefix = &strip_prefix;
        let cwd = std::path::PathBuf::from(&cwd);
        let crash_abs = &crash_abs;
        // PRYLINT_SHARD_PROBE=N (debug aid): simulate N-way phase-2 sharding
        // in ONE process — engine k boots ALL files (mirroring pylint's
        // phase-1 cache state) but walks only its round-robin residue class.
        // Output then flows through the normal in-order merge: byte-comparing
        // against the sequential run proves (or refutes) that per-worker
        // cache divergence is output-invisible.
        let shard_probe: usize = std::env::var("PRYLINT_SHARD_PROBE")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&n| n >= 2)
            .unwrap_or(1);
        let engine_thread = std::thread::Builder::new()
            .stack_size(1 << 30)
            .spawn_scoped(scope, move || {
                let mut all: Vec<(usize, ModuleOut)> = Vec::new();
                for k in 0..shard_probe {
                    let engine = pyinfer::Engine::new(&cwd);
                    for p in crash_abs {
                        engine.add_crash_file(p.clone());
                    }
                    // linter._pragma_lineno: run-global, updated in module
                    // order by the sequential pragma scans (C0302 leak)
                    let mut pragma_lineno: rustc_hash::FxHashMap<String, u32> =
                        rustc_hash::FxHashMap::default();
                    let mut lint_run = pycheckers::walker::LintRun::default();
                    let mut oracle_proc = oracle::OracleProc::default();
                    // boot the import-oracle interpreter NOW so it loads while
                    // the engine works; first query then responds immediately
                    oracle_proc.prespawn();
                    // pylint phase 1 builds EVERY file's AST through the astroid
                    // manager in file order (pylinter.get_ast -> ast_from_file
                    // source=True); the cache state at check time depends on it.
                    // Crash-marked files are skipped: astroid's attempt failed
                    // and cached nothing.
                    let mods: Vec<Option<pyinfer::ModId>> = items
                        .iter()
                        .map(|it| {
                            engine
                                .ast_from_file(&it.filepath, Some(&it.name), false, true)
                                .ok()
                        })
                        .collect();
                    engine.reset_crash_trip();
                    phase_mark!(t0, "engine-boot");
                    if std::env::var("PRYLINT_BOOT_ONLY").is_ok() {
                        // debug aid: report boot-graph footprint and stop
                        eprintln!("[phase] boot-only abort");
                        std::process::exit(0);
                    }
                    // PRYLINT_WALK_PREFIX=m + PRYLINT_WALK_PLUS=substr (debug
                    // aid, requires PRYLINT_SHARD_PROBE>=2 path): walk only
                    // tree positions < m plus paths containing substr.
                    let walk_prefix: Option<usize> =
                        std::env::var("PRYLINT_WALK_PREFIX").ok().and_then(|v| v.parse().ok());
                    let walk_plus = std::env::var("PRYLINT_WALK_PLUS").ok();
                    if k == 0 && std::env::var("PRYLINT_LIST_POS").is_ok() {
                        for (pos, (i, _)) in trees.iter().enumerate() {
                            eprintln!("[pos] {pos} {}", items[*i].filepath);
                        }
                    }
                    all.extend(trees.iter().enumerate().filter(|(pos, (i, _))| {
                        match (&walk_prefix, &walk_plus) {
                            (None, _) => pos % shard_probe == k,
                            (Some(m), plus) => {
                                k == 0
                                    && (pos < m
                                        || plus
                                            .as_ref()
                                            .is_some_and(|s| items[*i].filepath.contains(s.as_str())))
                            }
                        }
                    }).map(
                        |(_, (i, p))| {
                            (
                                *i,
                                lint_tree(
                                    &items[*i],
                                    p,
                                    global,
                                    strip_prefix,
                                    unicode_prepared,
                                    &prep,
                                    &engine,
                                    mods[*i],
                                    &mut lint_run,
                                    &mut oracle_proc,
                                    &mut pragma_lineno,
                                ),
                            )
                        },
                    ));
                    // ---- checker close() phase ----
                    // reversed(prepare_checkers) order: "similarities" sorts
                    // after "imports" alphabetically, so in REVERSED order
                    // SimilaritiesChecker.close() (R0801) runs BEFORE
                    // ImportsChecker.close() (R0401). Both attribute to the
                    // LAST linted module (current_name) at line 1 col 0; equal
                    // index entries keep push order under the stable merge
                    // sort, so R0801 (pushed first) precedes R0401 in output.
                    //
                    // SimilaritiesChecker.close() (R0801 duplicate-code).
                    if prep.sim_kept && k == 0 {
                        let idx = store().by_msgid["R0801"];
                        // close()-time enablement is CONFIG-level (line=None
                        // path): per-file pragmas can't suppress here (they
                        // dropped lines at collection time).
                        let enabled = is_message_enabled(global, None, idx, None);
                        let mut bodies: Vec<(usize, String)> = Vec::new();
                        let (_dup, _total) =
                            lint_run.similarities.close(&mut |nfiles, body| {
                                bodies.push((nfiles, body));
                            });
                        if enabled && !bodies.is_empty() {
                            if let Some(&(i, _)) =
                                trees.iter().map(|(i, p)| (*i, p)).collect::<Vec<_>>().last()
                            {
                                let item = &items[i];
                                // R0801 is a node-LESS message: module/abspath
                                // = linter.current_name/current_file of the
                                // LAST linted module (the raw FileItem name,
                                // matching the cyclic-import R0401 attribution
                                // below).
                                let module = item.name.clone();
                                let path = discover::absolute(&item.filepath)
                                    .replacen(strip_prefix.as_str(), "", 1);
                                let mut out = ModuleOut {
                                    msgs: Vec::new(),
                                    stats: Stats::default(),
                                    linted: false,
                                };
                                for (nfiles, body) in bodies {
                                    out.stats.count("R0801");
                                    out.msgs.push(OutMsg {
                                        module: module.clone(),
                                        path: path.clone(),
                                        line: 1,
                                        col: 0,
                                        msgid: "R0801",
                                        symbol: "duplicate-code",
                                        text: format!("Similar lines in {nfiles} files\n{body}"),
                                    });
                                }
                                all.push((i, out));
                            }
                        }
                    }
                    // ---- checker close() phase: R0401 cyclic-import
                    // (imports.py:484-490). Attributed to the last checked
                    // module; enablement is CONFIG-level (line=None path) —
                    // per-edge pragma suppression already happened at graph
                    // build time (probed on the pinned venv).
                    if prep.full && k == 0 {
                        let idx = store().by_msgid["R0401"];
                        if is_message_enabled(global, None, idx, None) {
                            let mut texts: Vec<String> = Vec::new();
                            lint_run.imports.close_cycles(&mut |t| texts.push(t));
                            if !texts.is_empty() {
                                let mut out = ModuleOut {
                                    msgs: Vec::new(),
                                    stats: Stats::default(),
                                    linted: false,
                                };
                                if let Some(&(i, _)) =
                                    trees.iter().map(|(i, p)| (*i, p)).collect::<Vec<_>>().last()
                                {
                                    let item = &items[i];
                                    let module = item
                                        .name
                                        .clone();
                                    let path = discover::absolute(&item.filepath)
                                        .replacen(strip_prefix.as_str(), "", 1);
                                    for t in texts {
                                        out.stats.count("R0401");
                                        out.msgs.push(OutMsg {
                                            module: module.clone(),
                                            path: path.clone(),
                                            line: 1,
                                            col: 0,
                                            msgid: "R0401",
                                            symbol: "cyclic-import",
                                            text: format!("Cyclic import ({t})"),
                                        });
                                    }
                                    all.push((i, out));
                                }
                            }
                        }
                    }
                }
                all
            })
            .expect("spawn phase-2 thread");

        // oracle verdicts (subprocess runs concurrently with the engine)
        let verdicts = oracle::run_oracle(&requests);
        for (&i, verdict) in oracle_idx.iter().zip(verdicts.into_iter()) {
            match verdict {
                Verdict::SyntaxError { line, offset, msg } => {
                    // `line or 1` / `col_offset or 0` (pylinter.py:1277-1278)
                    phase1.push((
                        i,
                        "E0001",
                        if line == 0 { 1 } else { line },
                        offset.unwrap_or(0),
                        msg,
                    ));
                }
                Verdict::ParseError { msg } => {
                    phase1.push((i, "F0010", 1, 0, format!("error while code parsing: {msg}")));
                }
                Verdict::AstroidError => {
                    // F0002 astroid-error: "%s: %s" with the crash-report
                    // message (lint/utils.py:107-112); timestamped template
                    // path under PYLINT_HOME (normalized by bytecmp.py).
                    phase1.push((i, "F0002", 1, 0, fatal_error_text(&items[i].filepath)));
                }
                Verdict::Ok { tokenize: Some(t) } => {
                    extras.push((
                        i,
                        lint_tokenize_err(&items[i], t.line, t.col, &t.msg, global, strip_prefix),
                    ));
                }
                Verdict::Ok { tokenize: None } => {
                    eprintln!(
                        "prylint: {} parses with CPython but not with ruff; module skipped",
                        items[i].filepath
                    );
                    // astroid parses + tokenizes but ruff rejected: pylint
                    // would lint it; we cannot (no tree). Counts as linted,
                    // emits nothing.
                    extras.push((i, ModuleOut { msgs: Vec::new(), stats: Stats::default(), linted: true }));
                }
            }
        }

        // ---- flush phase-1 messages in file order (before any phase-2
        // output is written; the engine results are merged below) ---------
        phase1.sort_by_key(|(i, ..)| *i);
        for (i, msgid, line, col, text) in &phase1 {
            let idx = store().by_msgid[msgid];
            // base FileState during phase 1: global state only
            if !is_message_enabled(global, None, idx, Some((*line).max(0) as u32)) {
                continue;
            }
            stats.count(msgid);
            reporter.handle(&OutMsg {
                module: items[*i].name.clone(),
                // node-less: abspath = current_file = FileItem.filepath
                // as discovered (relative): cwd strip is a no-op
                path: items[*i].filepath.replacen(strip_prefix, "", 1),
                line: *line,
                col: *col,
                msgid,
                symbol: msgstore::def(idx).symbol,
                text: text.clone(),
            });
        }

        let r = engine_thread.join().expect("phase-2 thread panicked");
        phase_mark!(t0, "phase2");
        r
    });

    // merge engine results with oracle-derived extras, in file order
    let mut merged: Vec<(usize, ModuleOut)> = results;
    merged.extend(extras);
    merged.sort_by_key(|(i, _)| *i);

    let mut any_linted = false;
    for (_, r) in &merged {
        any_linted |= r.linted;
        stats.fatal += r.stats.fatal;
        stats.error += r.stats.error;
        stats.warning += r.stats.warning;
        stats.refactor += r.stats.refactor;
        stats.convention += r.stats.convention;
        stats.info += r.stats.info;
        stats.statements += r.stats.statements;
        for m in &r.msgs {
            reporter.handle(m);
        }
    }
    reporter.flush();

    // ---- exit code (run.py:245-260 + _report_evaluation) ---------------
    let msg_status = reporter.msg_status;
    let score: Option<f64> = if !any_linted || stats.statements == 0 {
        None
    } else if stats.fatal > 0 {
        Some(0.0)
    } else {
        let penalty = (5 * stats.error + stats.warning + stats.refactor + stats.convention) as f64;
        Some((10.0 - penalty / stats.statements as f64 * 10.0).max(0.0))
    };
    match score {
        None => msg_status,
        Some(s) if s >= 10.0 => 0,
        Some(_) => {
            if msg_status != 0 {
                msg_status
            } else {
                1
            }
        }
    }
}

/// Phase-1 work for one file: read, decode, parse (pylinter.get_ast /
/// astroid file_build error taxonomy, builder.py:113-149).
fn parse_one(
    item: &FileItem,
    unicode_prepared: bool,
    format_prepared: bool,
    misc_prepared: bool,
    refac_prepared: bool,
    refac_r1707: bool,
    sim_prepared: bool,
) -> FileData {
    // modutils.get_source_file: a .pyi argument resolves to the sibling .py
    // source when it exists (PY_SOURCE_EXTS order, prefer_stubs=False)
    let mut read_path = item.filepath.clone();
    if let Some(base) = item.filepath.strip_suffix(".pyi") {
        let py = format!("{base}.py");
        if std::path::Path::new(&py).exists() {
            read_path = py;
        }
    }
    // error messages embed os.path.abspath of the resolved source path
    let abspath = discover::absolute(&read_path);
    let bytes = match std::fs::read(&read_path) {
        Ok(b) => b,
        Err(e) => {
            // OSError in open_source_file -> AstroidBuildingError ->
            // F0010 "Unable to load file {path}:\n{error}" (builder.py:119-125)
            return FileData::Phase1 {
                msgid: "F0010",
                line: 1,
                col: 0,
                text: format!("error while code parsing: Unable to load file {abspath}:\n{e}"),
            };
        }
    };
    let src = match decode_source(&bytes, &abspath) {
        Ok(src) => src,
        // SyntaxError/LookupError from detect_encoding -> AstroidSyntaxError
        // -> E0001 (lineno None -> 0 -> displayed 1; offset None -> 0)
        Err(DecodeError::Syntax(msg)) | Err(DecodeError::Lookup(msg)) => {
            return FileData::Phase1 {
                msgid: "E0001",
                line: 1,
                col: 0,
                text: format!("Parsing failed: '{msg}'"),
            };
        }
        // UnicodeError -> AstroidBuildingError -> F0010 (builder.py:135-139)
        Err(DecodeError::Unicode) => {
            return FileData::Phase1 {
                msgid: "F0010",
                line: 1,
                col: 0,
                text: format!("error while code parsing: Wrong or no encoding specified for {abspath}."),
            };
        }
    };
    // astroid _data_build modname handling (builder.py:200-208)
    let (modname, package) = if let Some(stripped) = item.name.strip_suffix(".__init__") {
        (stripped.to_string(), true)
    } else {
        let stem_is_init = std::path::Path::new(&read_path)
            .file_stem()
            .map(|s| s == "__init__")
            .unwrap_or(false);
        (item.name.clone(), stem_is_init)
    };
    let outcome = pyast::parse::parse_module(&src, &modname, &abspath, package);
    match outcome.tree {
        Some(tree) => {
            // unicode raw checker on the same bytes (pure; replayed in
            // phase 2 in this order). Pylint runs raw checkers only for
            // modules that reach the check phase, so parse failures above
            // never get here — same as before.
            let mut unicode_msgs = Vec::new();
            if unicode_prepared {
                pycheckers::unicode::process_module(&bytes, &mut |um| unicode_msgs.push(um));
            }
            // token-layer checkers (full mode): pylint tokenizes the RAW
            // byte stream — line endings preserved, unlike `src.text`.
            // C0302's line is provisional (resolved against the run-global
            // _pragma_lineno map at flush).
            let mut fmt_msgs = Vec::new();
            let mut fixme_msgs = Vec::new();
            let mut refac_tokens = pycheckers::refactoring::RefacTokens::default();
            if format_prepared || misc_prepared || refac_prepared {
                if let Some(raw) = pyast::decode_source_raw(&bytes, &abspath) {
                    let tk = pyast::pytok::tokenize_for_checkers(&raw);
                    if format_prepared {
                        let empty = rustc_hash::FxHashMap::default();
                        pycheckers::format::process_tokens(&raw, &tk, &empty, &mut |m| {
                            fmt_msgs.push(m)
                        });
                    }
                    if misc_prepared {
                        pycheckers::miscck::process_tokens(&raw, &tk, &mut |m| {
                            fixme_msgs.push(m)
                        });
                    }
                    if refac_prepared {
                        // file-start R1707 state = config-level enablement
                        // (is_message_enabled(..., line=None)); the in-file
                        // enable-pragma rescan flips per-token after that.
                        refac_tokens =
                            pycheckers::refactoring::process_tokens(&tk, &raw, refac_r1707);
                    }
                }
            }
            // SimilaritiesChecker (R0801): process_module decodes the binary
            // stream via file_encoding + readlines(). decode_source_raw
            // already applies the cookie encoding + BOM strip for utf-8-sig
            // and preserves \r\n/\r; readlines() then splits keepends. A
            // decode failure -> empty lineset (UnicodeDecodeError bail-out).
            let sim_lines = if sim_prepared {
                Some(match pyast::decode_source_raw(&bytes, &abspath) {
                    Some(raw) => pycheckers::similarities::readlines(&raw),
                    None => Vec::new(),
                })
            } else {
                None
            };
            FileData::Parsed(Box::new(ParsedFile {
                tree,
                src,
                tokens: outcome.tokens,
                abspath,
                unicode_msgs,
                fmt_msgs,
                fixme_msgs,
                refac_tokens,
                sim_lines,
            }))
        }
        None => FileData::NeedsOracle,
    }
}

/// Phase-2 outcome for a ruff-rejected file where pylint's tokenize raised
/// TokenError (E0001 tokenize form, no prefix). Engine-less by construction.
fn lint_tokenize_err(
    item: &FileItem,
    line: i64,
    col: i64,
    msg: &str,
    global: &GlobalState,
    strip_prefix: &str,
) -> ModuleOut {
    let mut out = ModuleOut { msgs: Vec::new(), stats: Stats::default(), linted: true };
    // tokenize.TokenError -> E0001 with args = ex.args[0] verbatim
    // (NO "Parsing failed:" prefix), line/col from ex.args[1]
    // (pylinter.py:1080-1090); module skipped afterwards
    let idx = store().by_msgid["E0001"];
    // fresh FileState exists in pylint here, but with no pragmas
    // processed the lookup reduces to global state
    if is_message_enabled(global, None, idx, Some(line.max(0) as u32)) {
        out.stats.count("E0001");
        out.msgs.push(OutMsg {
            module: item.name.clone(),
            // current_file was reset to module.file (absolute)
            path: discover::absolute(&item.filepath).replacen(strip_prefix, "", 1),
            line: if line == 0 { 1 } else { line },
            col,
            msgid: "E0001",
            symbol: "syntax-error",
            text: msg.to_string(),
        });
    }
    out
}

/// Phase-2 work for one parsed module (`_lint_file` + `_check_astroid_module`,
/// pylinter.py:798-830 / 1062-1106).
#[allow(clippy::too_many_arguments)]
fn lint_tree(
    item: &FileItem,
    p: &ParsedFile,
    global: &GlobalState,
    strip_prefix: &str,
    unicode_prepared: bool,
    prep: &pycheckers::walker::Prepared,
    engine: &pyinfer::Engine,
    emod: Option<pyinfer::ModId>,
    lint_run: &mut pycheckers::walker::LintRun,
    oracle_proc: &mut oracle::OracleProc,
    pragma_lineno: &mut rustc_hash::FxHashMap<String, u32>,
) -> ModuleOut {
    let mut out = ModuleOut { msgs: Vec::new(), stats: Stats::default(), linted: true };
    {
        {
            let mut fs = FileState::new(&p.tree);
            let module = item.name.clone();
            let path = p.abspath.replacen(strip_prefix, "", 1);
            let mut msgs: Vec<OutMsg> = Vec::new();
            let mut stats = Stats::default();
            // FileState._ignored_msgs: (msgid, pragma line) -> message lines
            // (file_state.py:41-43); RefCell since the walker borrows fs
            // immutably while recording.
            let ignored_msgs: std::cell::RefCell<
                indexmap::IndexMap<(msgstore::MsgIdx, u32), std::collections::BTreeSet<u32>>,
            > = std::cell::RefCell::new(indexmap::IndexMap::new());
            let record_ignored = |fs: &FileState,
                                  idx: msgstore::MsgIdx,
                                  line: u32| {
                // handle_ignored_message: MSG_STATE_SCOPE_MODULE only
                if fs.state_scope_is_module(idx, line) {
                    if let Some(&orig) = fs.suppression_mapping.get(&(idx, line)) {
                        ignored_msgs
                            .borrow_mut()
                            .entry((idx, orig))
                            .or_default()
                            .insert(line);
                    }
                }
            };
            {
                let mut add = |fs: &mut FileState, msgid: &str, line: u32, col: i64, text: String| {
                    let Some(&idx) = store().by_msgid.get(msgid) else { return };
                    if !is_message_enabled(global, Some(fs), idx, Some(line)) {
                        record_ignored(fs, idx, line);
                        return;
                    }
                    stats.count(msgid);
                    msgs.push(OutMsg {
                        module: module.clone(),
                        path: path.clone(),
                        line: if line == 0 { 1 } else { line as i64 },
                        col,
                        msgid: msgstore::def(idx).msgid,
                        symbol: msgstore::def(idx).symbol,
                        text,
                    });
                };
                // 1. pragmas (PyLinter.process_tokens); skip-file aborts
                //    before raw checkers and the walker
                let mut pragma_sink = |fs: &mut FileState, msgid: &str, line: u32, text: String| {
                    add(fs, msgid, line, 0, text);
                };
                let ignore_file =
                    process_tokens(&p.tokens, &p.src, &mut fs, pragma_lineno, &mut pragma_sink);
                if !ignore_file {
                    // 2. raw checkers in sorted-name order: format (no-op),
                    //    miscellaneous (no-op), similarities (not ported),
                    //    string (not ported), unicode_checker — results
                    //    precomputed in phase 1, replayed here
                    if unicode_prepared {
                        for um in &p.unicode_msgs {
                            let text = store()
                                .by_msgid
                                .get(um.msgid)
                                .map(|&i| msgstore::def(i).template.to_string())
                                .unwrap_or_default();
                            add(&mut fs, um.msgid, um.line, um.col as i64, text);
                        }
                    }
                    // SimilaritiesChecker.process_module (R0801): collect this
                    // module's LineSet (no per-module emission; close() at
                    // end-of-run does the work). Runs only for pure_python,
                    // non-skip-file modules — both guaranteed here (emod parsed
                    // OK, !ignore_file). lineset.name = the .__init__-stripped
                    // module name (== linter.current_name).
                    if prep.sim_kept {
                        if let (Some(lines), Some(mid)) = (p.sim_lines.clone(), emod) {
                            let r0801_idx = store().by_msgid["R0801"];
                            let import_lines =
                                pycheckers::similarities::import_lines_of(engine, mid);
                            let signature_lines =
                                pycheckers::similarities::signature_lines_of(engine, mid);
                            // lineset.name = linter.current_name = the RAW
                            // FileItem name (NOT .__init__-stripped). When the
                            // run target is `.` an __init__ module's name is
                            // e.g. `tornado.__init__` and that is what appears
                            // in the ==name:[s:e] header.
                            let sim_name = item.name.clone();
                            let is_line_enabled = |lineno: u32| -> bool {
                                is_message_enabled(global, Some(&fs), r0801_idx, Some(lineno))
                            };
                            lint_run.similarities.process_module(
                                &sim_name,
                                lines,
                                &import_lines,
                                &signature_lines,
                                &is_line_enabled,
                            );
                        }
                    }
                    // 2b. TOKEN checkers in sorted-name order: format,
                    //     miscellaneous (refactoring/spelling/string not in
                    //     this phase). Messages precomputed in phase 1.
                    for fm in &p.fmt_msgs {
                        if fm.ignored_only {
                            // C0301 checker_off: linter.add_ignored_message
                            if let Some(&idx) = store().by_msgid.get(fm.msgid) {
                                record_ignored(&fs, idx, fm.line);
                            }
                            continue;
                        }
                        // C0302's line attribution comes from the RUN-GLOBAL
                        // _pragma_lineno map (state as of THIS module's scan)
                        let line = if fm.msgid == "C0302" {
                            ["C0302", "too-many-lines"]
                                .iter()
                                .filter_map(|k| pragma_lineno.get(*k))
                                .find(|&&l| l != 0)
                                .copied()
                                .unwrap_or(1)
                        } else {
                            fm.line
                        };
                        add(&mut fs, fm.msgid, line, fm.col, fm.text.clone());
                    }
                    for xm in &p.fixme_msgs {
                        add(&mut fs, "W0511", xm.line, xm.col, xm.text.clone());
                    }
                    // RefactoringChecker token pass (sorts after Format +
                    // Encoding in token-checker order). Populate _elifs for
                    // the AST walk (EXTEND, mirroring self._elifs.extend; the
                    // leak across modules when R1732 disabled is automatic —
                    // leave_module clears only when R1732 enabled) and emit
                    // R1707 (line= only, col 0, HIGH confidence).
                    lint_run.refac.elifs.extend(p.refac_tokens.elifs.iter().copied());
                    for &line in &p.refac_tokens.r1707_lines {
                        add(&mut fs, "R1707", line, 0, "Disallow trailing comma tuple".to_string());
                    }
                    // 3. AST walk: ImportsChecker + VariablesChecker wired
                    //    (walk_order.rs dispatch); other checkers pending.
                    //    The walker also accumulates nbstatements
                    //    (ast_walker.py:79-80).
                    let crashed = std::cell::Cell::new(false);
                    if let Some(mid) = emod {
                        engine.reset_crash_trip();
                        // node msgs use the astroid module name (`.__init__`
                        // stripped); node-LESS msgs (E0001 Cannot-import) use
                        // the raw FileItem name (notes/02; GT: core
                        // homeassistant.auth.__init__ vs homeassistant.auth)
                        let stripped: String = item
                            .name
                            .strip_suffix(".__init__")
                            .unwrap_or(&item.name)
                            .to_string();
                        let fs_ref = &fs;
                        let stats_ref = &mut stats;
                        let msgs_ref = &mut msgs;
                        let record_ignored_ref = &record_ignored;
                        let mut emit = |m: pycheckers::walker::CheckMsg| {
                            let Some(&idx) = store().by_msgid.get(m.msgid) else { return };
                            if !is_message_enabled(global, Some(fs_ref), idx, Some(m.line)) {
                                record_ignored_ref(fs_ref, idx, m.line);
                                return;
                            }
                            stats_ref.count(m.msgid);
                            // node messages take module/path from
                            // node.root() (pylinter.py:1257-1263): anchors
                            // living in ANOTHER module's tree are attributed
                            // to THAT module (numpy E0603 on inferred
                            // __all__ elements)
                            let (msg_module, msg_path) = match m.root_mid {
                                Some(rmid) if rmid != mid => {
                                    let rmd = engine.md(rmid);
                                    (
                                        rmd.name.clone(),
                                        rmd.file.replacen(strip_prefix, "", 1),
                                    )
                                }
                                _ => (
                                    if m.nodeless {
                                        item.name.clone()
                                    } else {
                                        stripped.clone()
                                    },
                                    path.clone(),
                                ),
                            };
                            msgs_ref.push(OutMsg {
                                module: msg_module,
                                path: msg_path,
                                line: if m.line == 0 { 1 } else { m.line as i64 },
                                col: m.col,
                                msgid: msgstore::def(idx).msgid,
                                symbol: msgstore::def(idx).symbol,
                                text: m.text,
                            });
                        };
                        let mut import_oracle = |p: &str, modname: &str| {
                            oracle_proc.syntax_error_str(p, modname)
                        };
                        let is_enabled = |msgid: &str, line: u32| -> bool {
                            match store().by_msgid.get(msgid) {
                                Some(&idx) => {
                                    is_message_enabled(global, Some(fs_ref), idx, Some(line))
                                }
                                None => true, // unknown ids treated enabled
                            }
                        };
                        // line=None path: config-level state only
                        let cfg_enabled = |msgid: &str| -> bool {
                            match store().by_msgid.get(msgid) {
                                Some(&idx) => global.enabled(idx),
                                None => true,
                            }
                        };
                        let add_ignored = |msgid: &str, line: u32| {
                            // linter.add_ignored_message (pylinter.py:1320-44)
                            if let Some(&idx) = store().by_msgid.get(msgid) {
                                record_ignored_ref(fs_ref, idx, line);
                            }
                        };
                        lint_run.walk_module(
                            engine,
                            mid,
                            prep,
                            &mut emit,
                            &mut import_oracle,
                            &is_enabled,
                            &cfg_enabled,
                            &add_ignored,
                            &crashed,
                        );
                        if engine.crash_tripped() {
                            crashed.set(true);
                            engine.reset_crash_trip();
                        }
                    }
                    stats.statements += count_statements(&p.tree);
                    if crashed.get() {
                        // the module check CRASHED (pylint: exception out of
                        // check_astroid_module -> AstroidError caught in
                        // _lint_files -> F0002, spurious-suppression step
                        // skipped). Messages emitted before the crash stay.
                        let idx = store().by_msgid["F0002"];
                        if is_message_enabled(global, Some(&fs), idx, None) {
                            stats.count("F0002");
                            msgs.push(OutMsg {
                                module: item.name.clone(),
                                path: path.clone(),
                                line: 1,
                                col: 0,
                                msgid: msgstore::def(idx).msgid,
                                symbol: msgstore::def(idx).symbol,
                                text: fatal_error_text(&item.filepath),
                            });
                        }
                        out.msgs = msgs;
                        out.stats = stats;
                        return out;
                    }
                    // ---- iter_spurious_suppression_messages ----
                    // (file_state.py:225-256; added via add_message AFTER
                    //  check_astroid_module, pylinter.py:825-830)
                    let mut spurious: Vec<(&'static str, u32, String)> = Vec::new();
                    for (&idx, lines) in fs.raw_state.iter() {
                        let def = msgstore::def(idx);
                        for &(line, enable) in lines {
                            if !enable
                                && !ignored_msgs.borrow().contains_key(&(idx, line))
                                && !crate::msgstate::INCOMPATIBLE_WITH_USELESS_SUPPRESSION
                                    .contains(&def.msgid)
                            {
                                spurious.push((
                                    "I0021",
                                    line,
                                    format!("Useless suppression of '{}'", def.symbol),
                                ));
                            }
                        }
                    }
                    // I0020 suppressed-message (list() snapshot)
                    let snapshot: Vec<((msgstore::MsgIdx, u32), Vec<u32>)> = ignored_msgs
                        .borrow()
                        .iter()
                        .map(|(k, v)| (*k, v.iter().copied().collect()))
                        .collect();
                    for ((widx, from), lines) in snapshot {
                        let wdef = msgstore::def(widx);
                        for line in lines {
                            spurious.push((
                                "I0020",
                                line,
                                format!("Suppressed '{}' (from line {})", wdef.symbol, from),
                            ));
                        }
                    }
                    for (msgid, line, text) in spurious {
                        let Some(&idx) = store().by_msgid.get(msgid) else { continue };
                        if !is_message_enabled(global, Some(&fs), idx, Some(line)) {
                            record_ignored(&fs, idx, line);
                            continue;
                        }
                        stats.count(msgid);
                        msgs.push(OutMsg {
                            module: module.clone(),
                            path: path.clone(),
                            line: if line == 0 { 1 } else { line as i64 },
                            col: 0,
                            msgid: msgstore::def(idx).msgid,
                            symbol: msgstore::def(idx).symbol,
                            text,
                        });
                    }
                }
            }
            out.msgs = msgs;
            out.stats = stats;
            out
        }
    }
}

/// PYLINT_HOME (pylint/constants.py): $PYLINT_HOME else the platform
/// user-cache dir for "pylint" (macOS ~/Library/Caches/pylint, linux
/// $XDG_CACHE_HOME/pylint or ~/.cache/pylint).
fn pylint_home() -> String {
    if let Ok(h) = std::env::var("PYLINT_HOME") {
        if !h.is_empty() {
            return h;
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    if cfg!(target_os = "macos") {
        format!("{home}/Library/Caches/pylint")
    } else {
        match std::env::var("XDG_CACHE_HOME") {
            Ok(x) if !x.is_empty() => format!("{x}/pylint"),
            _ => format!("{home}/.cache/pylint"),
        }
    }
}

/// prepare_crash_report's issue-template path: PYLINT_HOME /
/// datetime.now().strftime("pylint-crash-%Y-%m-%d-%H-%M-%S.txt").
/// Wall-clock dependent; harness/bytecmp.py normalizes the timestamp for
/// byte comparison. We do not write the template file.
fn crash_file_path() -> String {
    let ts = std::process::Command::new("date")
        .arg("+%Y-%m-%d-%H-%M-%S")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "1970-01-01-00-00-00".to_string());
    format!("{}/pylint-crash-{}.txt", pylint_home(), ts)
}

/// F0002 astroid-error text: "%s: %s" with
/// (filepath, get_fatal_error_message(filepath, template_path))
/// (lint/utils.py:107-112).
fn fatal_error_text(display_path: &str) -> String {
    format!(
        "{p}: Fatal error while checking '{p}'. Please open an issue in our bug tracker so we address this. There is a pre-filled template that you can use in '{c}'.",
        p = display_path,
        c = crash_file_path()
    )
}

/// max nesting depth of the built tree — a cheap stand-in for the python
/// recursion depth astroid's rebuilder needs. Builder allocates parents
/// before children, so a forward pass suffices.
fn tree_depth(tree: &Tree) -> u32 {
    let n = tree.nodes.len();
    let mut depth = vec![1u32; n];
    let mut maxd = 1;
    for i in 1..n {
        let p = tree.nodes[i].parent.idx();
        let d = if p < i { depth[p] + 1 } else { 2 };
        depth[i] = d;
        if d > maxd {
            maxd = d;
        }
    }
    maxd
}

/// astroid `is_statement` node kinds (subclasses of _base_nodes.Statement).
fn is_statement_kind(k: &NodeKind) -> bool {
    matches!(
        k,
        NodeKind::FunctionDef(_)
            | NodeKind::AsyncFunctionDef(_)
            | NodeKind::ClassDef(_)
            | NodeKind::Return { .. }
            | NodeKind::Delete { .. }
            | NodeKind::Assign { .. }
            | NodeKind::AugAssign { .. }
            | NodeKind::AnnAssign { .. }
            | NodeKind::TypeAlias { .. }
            | NodeKind::For(_)
            | NodeKind::AsyncFor(_)
            | NodeKind::While { .. }
            | NodeKind::If { .. }
            | NodeKind::With(_)
            | NodeKind::AsyncWith(_)
            | NodeKind::Match { .. }
            | NodeKind::Raise { .. }
            | NodeKind::Try(_)
            | NodeKind::TryStar(_)
            | NodeKind::Assert { .. }
            | NodeKind::Import { .. }
            | NodeKind::ImportFrom { .. }
            | NodeKind::Global { .. }
            | NodeKind::Nonlocal { .. }
            | NodeKind::Expr { .. }
            | NodeKind::Pass
            | NodeKind::Break
            | NodeKind::Continue
            | NodeKind::ExceptHandler { .. }
    )
}

fn count_statements(tree: &Tree) -> u64 {
    let mut count = 0u64;
    let mut stack: Vec<NodeId> = Vec::new();
    tree.push_children(NodeId::MODULE, &mut stack);
    while let Some(id) = stack.pop() {
        if is_statement_kind(tree.kind(id)) {
            count += 1;
        }
        tree.push_children(id, &mut stack);
    }
    count
}
