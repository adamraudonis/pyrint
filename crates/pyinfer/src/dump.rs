//! `--dump-infer` driver: mirrors harness/dump_infer.py exactly — phase 1
//! pre-builds every fileitem into the module graph in order, phase 2 dumps
//! inference results for every Name(Load)/Attribute(Load)/Call node in
//! preorder using render() compatible formatting.

use std::io::Write;
use std::rc::Rc;

use pyast::tree::{ConstValue, IntValue, NodeKind};

use crate::ctx::Ctx;
use crate::graph::Engine;
use crate::value::{Drive, ErrKind, GNode, ModId, SeqKind, Value};

pub fn run_dump_infer(items_path: &str) -> i32 {
    // Deep inference recursion (astroid runs with setrecursionlimit(8000))
    // needs far more than the default 8MB stack.
    let items_path = items_path.to_string();
    std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(move || run_dump_infer_inner(&items_path))
        .expect("spawn dump thread")
        .join()
        .unwrap_or(1)
}

/// `--dump-ancestors <items.jsonl>` debug driver. Prebuilds the corpus
/// exactly like the real run (phase 1, file order), then for every ClassDef
/// whose `file:line` matches an entry in PRYLINT_ANC_TARGETS (one
/// `path:lineno` per line) prints its ancestor count + the post-walk
/// nodes_inferred counter so the astroid `nodes_inferred>100` collapse can be
/// diffed. Debug-only: never touches the lint path.
pub fn run_dump_ancestors(items_path: &str) -> i32 {
    let items_path = items_path.to_string();
    std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(move || run_dump_ancestors_inner(&items_path))
        .expect("spawn dump-ancestors thread")
        .join()
        .unwrap_or(1)
}

fn run_dump_ancestors_inner(items_path: &str) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_default();
    let engine = Engine::new(&cwd);
    let data = std::fs::read_to_string(items_path).unwrap_or_default();
    let items: Vec<(String, String)> = data
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            Some((
                v["name"].as_str()?.to_string(),
                v["path"].as_str()?.to_string(),
            ))
        })
        .collect();
    let mut trees: Vec<Option<ModId>> = Vec::with_capacity(items.len());
    for (name, path) in &items {
        trees.push(engine.ast_from_file(path, Some(name), false, true).ok());
    }
    // targets: "path:lineno" per line
    let targets: Vec<(String, u32)> = std::env::var("PRYLINT_ANC_TARGETS")
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|d| {
            d.lines()
                .filter_map(|l| {
                    let l = l.trim();
                    if l.is_empty() {
                        return None;
                    }
                    let (p, ln) = l.rsplit_once(':')?;
                    Some((p.to_string(), ln.parse::<u32>().ok()?))
                })
                .collect()
        })
        .unwrap_or_default();
    for ((_name, path), tree) in items.iter().zip(trees.iter()) {
        let Some(mid) = tree else { continue };
        let md = engine.md(*mid);
        for n in engine.walk_preorder(*mid) {
            if !matches!(md.tree.nodes[n.idx()].kind, NodeKind::ClassDef(_)) {
                continue;
            }
            let g = GNode { m: *mid, n };
            let line = md.tree.nodes[n.idx()].fromlineno;
            let want = targets.is_empty()
                || targets.iter().any(|(p, ln)| path.ends_with(p.as_str()) && *ln == line);
            if !want {
                continue;
            }
            // full recursive ancestors with a SHARED context to observe the cap
            let ctx2 = Ctx::new();
            let mut ancs: Vec<GNode> = Vec::new();
            engine.ancestors_to(g, true, Some(&ctx2), &mut |a| {
                ancs.push(a);
                Drive::Go
            });
            // count_parents-style walk (design_analysis _get_parents_iter):
            // ancestors(recurs=false) work-list, each with a FRESH None ctx —
            // this is what the R0901 checker actually does, and it hits the
            // warm GLOBAL inference cache.
            let mut parents: rustc_hash::FxHashSet<GNode> = Default::default();
            let mut to_explore = engine.ancestors(g, false, None);
            while let Some(p) = to_explore.pop() {
                if parents.insert(p) {
                    to_explore.extend(engine.ancestors(p, false, None));
                }
            }
            println!(
                "{}:{}: {} recurs_ancestors={} ni={} count_parents={}",
                path,
                line,
                engine.qname(g),
                ancs.len(),
                ctx2.nodes_inferred.get(),
                parents.len(),
            );
        }
    }
    0
}

fn run_dump_infer_inner(items_path: &str) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_default();
    let engine = Engine::new(&cwd);
    let data = match std::fs::read_to_string(items_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("prylint: cannot read {items_path}: {e}");
            return 1;
        }
    };
    let items: Vec<(String, String)> = data
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            Some((
                v["name"].as_str()?.to_string(),
                v["path"].as_str()?.to_string(),
            ))
        })
        .collect();
    // Phase 1: build ALL target files into the graph, in order.
    let mut trees: Vec<Option<ModId>> = Vec::with_capacity(items.len());
    for (name, path) in &items {
        match engine.ast_from_file(path, Some(name), false, true) {
            Ok(id) => trees.push(Some(id)),
            Err(_) => trees.push(None),
        }
    }
    // Phase 2: dump. PRYLINT_DUMP_ONLY=<file of paths> restricts which files
    // are dumped while STILL prebuilding everything above (mirrors pylint
    // phase 1 + dump_infer.py --only; sampled runs otherwise miss cross-file
    // instance_attrs the oracle saw).
    let only: Option<rustc_hash::FxHashSet<String>> = std::env::var("PRYLINT_DUMP_ONLY")
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|d| {
            d.lines()
                .filter(|l| !l.trim().is_empty())
                .map(String::from)
                .collect()
        });
    // Debug aid: PRYLINT_TRACE_START=<path substring> turns PRYLINT_TRACE_INFER
    // on only once the dump loop reaches a matching file (keeps prefix-state
    // trace runs from emitting gigabytes for the warm-up files).
    let trace_start = std::env::var("PRYLINT_TRACE_START").ok();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    // PRYLINT_PROBE_COLD=<path_suffix>:<line>:<col>:<kind> — after the full
    // prebuild, infer ONLY this one node with a FRESH context (no preorder
    // warming) to measure the genuine cold result + nodes_inferred, mirroring
    // what a checker's safe_infer sees as the first inference of the subtree.
    if let Ok(spec) = std::env::var("PRYLINT_PROBE_COLD") {
        let parts: Vec<&str> = spec.rsplitn(3, ':').collect(); // [col, line, path]
        if parts.len() == 3 {
            let want_path = parts[2];
            let want_line: u32 = parts[1].parse().unwrap_or(0);
            let want_col: u32 = parts[0].parse().unwrap_or(0);
            for ((_n, path), tree) in items.iter().zip(trees.iter()) {
                if !path.ends_with(want_path) {
                    continue;
                }
                let Some(mid) = tree else { continue };
                let want_kind = std::env::var("PRYLINT_PROBE_KIND").unwrap_or_default();
                for n in engine.walk_preorder(*mid) {
                    let md = engine.md(*mid);
                    let ni = &md.tree.nodes[n.idx()];
                    let kname = match &ni.kind {
                        NodeKind::Name { .. } => "Name",
                        NodeKind::Attribute { .. } => "Attribute",
                        NodeKind::Call { .. } => "Call",
                        _ => "Other",
                    };
                    if ni.fromlineno == want_line
                        && ni.col_offset as u32 == want_col
                        && (want_kind.is_empty() || want_kind == kname)
                    {
                        let g = GNode { m: *mid, n };
                        for rep in 0..3 {
                            let ctx = Ctx::new();
                            let flow = engine.infer(g, &ctx);
                            let rendered: Vec<String> =
                                flow.vals.iter().map(|v| render(&engine, v)).collect();
                            eprintln!(
                                "PROBE_COLD#{} {}:{}:{}:{} -> {} ##{} err={:?}",
                                rep, path, ni.fromlineno, ni.col_offset, kname,
                                rendered.join(" | "),
                                ctx.nodes_inferred.get(),
                                flow.err,
                            );
                        }
                    }
                }
            }
        }
    }
    for ((_name, path), tree) in items.iter().zip(trees.iter()) {
        if let Some(set) = &only {
            if !set.contains(path.as_str()) {
                continue;
            }
        }
        if let Some(ts) = &trace_start {
            if path.contains(ts.as_str()) {
                crate::graph::set_trace_infer(true);
            }
        }
        let _ = writeln!(out, "=== {path}");
        let Some(mid) = tree else {
            let _ = writeln!(out, "NOTREE");
            continue;
        };
        dump_module(&engine, *mid, &mut out);
    }
    0
}

fn dump_module<W: Write>(engine: &Engine, mid: ModId, out: &mut W) {
    let md = engine.md(mid);
    // dump_infer.py does print("\n".join(out)) — an empty file still
    // produces one blank line.
    let mut wrote_any = false;
    for n in engine.walk_preorder(mid) {
        let interesting = matches!(
            md.tree.nodes[n.idx()].kind,
            NodeKind::Name { .. } | NodeKind::Attribute { .. } | NodeKind::Call { .. }
        );
        if !interesting {
            continue;
        }
        let g = GNode { m: mid, n };
        let kind_name = match &md.tree.nodes[n.idx()].kind {
            NodeKind::Name { .. } => "Name",
            NodeKind::Attribute { .. } => "Attribute",
            _ => "Call",
        };
        let node_info = &md.tree.nodes[n.idx()];
        if crate::graph::trace_infer() {
            // sentinel for trace alignment with the GT tracer's @@DUMPNODE
            eprintln!(
                "@@DUMPNODE {}:{}:{}",
                node_info.fromlineno, node_info.col_offset, kind_name
            );
        }
        let ctx = Ctx::new();
        let flow = engine.infer(g, &ctx);
        let rendered: Vec<String> = if let Some(e) = flow.err {
            vec![match e {
                ErrKind::Inference | ErrKind::NameError => "ERR".to_string(),
                ErrKind::Recursion => "RECURSION".to_string(),
                ErrKind::Attribute => "CRASH:AttributeInferenceError".to_string(),
                other => format!("CRASH:{other:?}"),
            }]
        } else {
            flow.vals.iter().map(|v| render(engine, v)).collect()
        };
        // debug aid: append the per-node nodes_inferred counter so bump
        // dynamics can be diffed against the python oracle
        // (harness/dump_infer_count.py).
        let counts = std::env::var("PRYLINT_DUMP_COUNTS").is_ok();
        if counts {
            let _ = writeln!(
                out,
                "{}:{}:{} -> {} ##{}",
                node_info.fromlineno,
                node_info.col_offset,
                kind_name,
                rendered.join(" | "),
                ctx.nodes_inferred.get()
            );
        } else {
            let _ = writeln!(
                out,
                "{}:{}:{} -> {}",
                node_info.fromlineno,
                node_info.col_offset,
                kind_name,
                rendered.join(" | ")
            );
        }
        wrote_any = true;
    }
    if !wrote_any {
        let _ = writeln!(out);
    }
}

/// dump_infer.py render()
pub fn render(engine: &Engine, v: &Value) -> String {
    match v {
        Value::Uninferable => "U".to_string(),
        // dump_infer.py render() has no EvaluatedObject case — falls to
        // the Other:<type> tail
        Value::EvaluatedObject { .. } => "Other:EvaluatedObject".to_string(),
        Value::SynthConst(c) => render_const(c),
        Value::Node(g) => {
            let md = engine.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::Const(c) => render_const(c),
                NodeKind::ClassDef(_) => format!("Class:{}", engine.qname(*g)),
                NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) => {
                    format!("Func:{}", engine.qname(*g))
                }
                NodeKind::Lambda(_) => {
                    // dump_infer.py renders v.root().name — REPARENT-AWARE:
                    // extension-template lambdas (multiprocessing.managers
                    // `__exit__ = lambda *args: args`) live in a ''-named
                    // template module whose classes are reparented into the
                    // real module (brain/helpers.py:25-27); root() walks
                    // through the override.
                    let root_name = {
                        let mut top = *g;
                        while let Some(p) = engine.parent(top) {
                            top = p;
                        }
                        &engine.md(top.m).name
                    };
                    format!(
                        "Lambda:{}:{}",
                        root_name,
                        md.tree.nodes[g.n.idx()].fromlineno
                    )
                }
                NodeKind::Module(_) => format!("Module:{}", md.name),
                NodeKind::List { elts, .. } => format!("List:{}", elts.len()),
                NodeKind::Tuple { elts, .. } => format!("Tuple:{}", elts.len()),
                NodeKind::Set { elts } => format!("Set:{}", elts.len()),
                NodeKind::Dict { items } => format!("Dict:{}", items.len()),
                NodeKind::Slice { .. } => "Slice".to_string(),
                NodeKind::Unknown => "Unknown".to_string(),
                NodeKind::EmptyNode => "Empty".to_string(),
                k => format!("Other:{}", crate::treeutil::kind_label(k)),
            }
        }
        Value::Property { func, synth } => {
            if *synth {
                "Prop:__astroid_synthetic.<property>".to_string()
            } else {
                format!("Prop:{}", engine.qname(*func))
            }
        }
        // objects.py:325 PartialFunction.qname() is the literal class name
        Value::Partial { .. } => "Partial:PartialFunction".to_string(),
        Value::ExcInst { cls, .. } => format!("ExcInst:{}", engine.qname(*cls)),
        Value::Super { .. } => "Super".to_string(),
        Value::FrozenSet { .. } => "Frozen".to_string(),
        Value::DictItems(_) => "DictItems".to_string(),
        Value::DictKeys(_) => "DictKeys".to_string(),
        Value::DictValues(_) => "DictValues".to_string(),
        Value::BoundMethod { func, .. } | Value::DescBM { func, .. } => {
            format!("BM:{}", engine.qname(*func))
        }
        Value::UnboundMethod { func } => format!("UM:{}", engine.qname(*func)),
        Value::Generator { func, is_async, call_ctx } => {
            // Generator.parent is the PartialFunction object when created
            // through a partial call; its qname() is "PartialFunction"
            let q = if engine
                .partial_gen_ctxs
                .borrow()
                .contains_key(&(std::rc::Rc::as_ptr(call_ctx) as usize))
            {
                "PartialFunction".to_string()
            } else {
                engine.qname(*func)
            };
            if *is_async {
                format!("AGen:{q}")
            } else {
                format!("Gen:{q}")
            }
        }
        Value::UnionType => "Union".to_string(),
        Value::SynthSeq { kind, elems } => match kind {
            SeqKind::List => format!("List:{}", elems.len()),
            SeqKind::Tuple => format!("Tuple:{}", elems.len()),
            SeqKind::Set => format!("Set:{}", elems.len()),
        },
        Value::SynthDict { items } => format!("Dict:{}", items.len()),
        Value::SynthSlice { .. } => "Slice".to_string(),
        Value::Inst { cls, .. } => format!("Inst:{}", engine.qname(*cls)),
    }
}

fn render_const(c: &ConstValue) -> String {
    let (type_name, repr) = const_type_and_repr(c);
    let truncated: String = repr.chars().take(40).collect();
    format!("Const:{type_name}:{truncated}")
}

fn const_type_and_repr(c: &ConstValue) -> (&'static str, String) {
    match c {
        ConstValue::None => ("NoneType", "None".to_string()),
        ConstValue::NotImplemented => ("NotImplementedType", "NotImplemented".to_string()),
        ConstValue::Ellipsis => ("ellipsis", "Ellipsis".to_string()),
        ConstValue::Bool(b) => ("bool", if *b { "True" } else { "False" }.to_string()),
        ConstValue::Int(IntValue::Small(i)) => ("int", i.to_string()),
        ConstValue::Int(IntValue::Big(s)) => ("int", s.to_string()),
        ConstValue::Float(f) => ("float", pyast::pyrepr::repr_float(*f)),
        ConstValue::Complex { real, imag } => ("complex", repr_complex(*real, *imag)),
        ConstValue::Str(s) => ("str", pyast::pyrepr::repr_str(s)),
        ConstValue::StrSurrogate(p) => ("str", pyast::pyrepr::repr_str_points(p)),
        ConstValue::Bytes(b) => ("bytes", repr_bytes(b)),
    }
}

fn repr_complex(real: f64, imag: f64) -> String {
    // CPython: complex repr; real==0 -> "1j" style
    if real == 0.0 && real.is_sign_positive() {
        format!("{}j", trim_float(imag))
    } else {
        let sign = if imag >= 0.0 || imag.is_nan() { "+" } else { "-" };
        format!(
            "({}{}{}j)",
            trim_float(real),
            sign,
            trim_float(imag.abs())
        )
    }
}

fn trim_float(f: f64) -> String {
    let r = pyast::pyrepr::repr_float(f);
    r.strip_suffix(".0").map(|s| s.to_string()).unwrap_or(r)
}

/// public alias for f-string `format(bytes, "")` folding (str(bytes) = repr)
pub fn repr_bytes_pub(b: &[u8]) -> String {
    repr_bytes(b)
}

fn repr_bytes(b: &[u8]) -> String {
    let has_single = b.contains(&b'\'');
    let has_double = b.contains(&b'"');
    let (quote, escape_quote) = if has_single && !has_double {
        ('"', b'"')
    } else {
        ('\'', b'\'')
    };
    let mut out = String::from("b");
    out.push(quote);
    for &byte in b {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            x if x == escape_quote => {
                out.push('\\');
                out.push(x as char);
            }
            0x20..=0x7e => out.push(byte as char),
            _ => out.push_str(&format!("\\x{byte:02x}")),
        }
    }
    out.push(quote);
    out
}

impl Engine {
    /// convenience for unit probes
    pub fn infer_fresh(&self, g: GNode) -> crate::value::Flow {
        let ctx: Rc<Ctx> = Ctx::new();
        self.infer(g, &ctx)
    }
}
