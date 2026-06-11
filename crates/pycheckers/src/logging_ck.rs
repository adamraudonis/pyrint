//! LoggingChecker (checkers/logging.py) — E1200/E1201/E1205/E1206 +
//! W1201/W1202/W1203 computations (disabled; resurrection + I0021).
//! Default config: logging-modules=("logging",), logging-format-style=old.

use pyast::tree::{ConstValue, NodeKind};
use pyinfer::graph::Engine;
use pyinfer::value::{GNode, Value};

use crate::ckutils as u;
use crate::strings::{parse_format_string, PctError};
use crate::walker::WalkCx;

const CHECKED_CONVENIENCE_FUNCTIONS: &[&str] = &[
    "critical", "debug", "error", "exception", "fatal", "info", "warn", "warning",
];
const MOST_COMMON_FORMATTING: &[&str] = &["%s", "%d", "%f", "%r"];

#[derive(Default)]
pub struct LoggingCk {
    /// names bound to logging modules in the CURRENT module
    logging_names: std::collections::HashSet<String>,
}

fn is_classdef(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_)))
}

impl LoggingCk {
    pub fn visit_module(&mut self, _cx: &mut WalkCx, _node: GNode) {
        self.logging_names.clear();
        // default logging_modules = ("logging",): _from_imports stays empty
    }

    pub fn visit_importfrom(&mut self, _cx: &mut WalkCx, _node: GNode) {
        // default config: _from_imports empty -> no-op
    }

    pub fn visit_import(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let md = eng.md(node.m);
        if let NodeKind::Import { names } = &md.tree.nodes[node.n.idx()].kind {
            for &(module, as_name) in names {
                let modname = md.tree.s(module).to_string();
                if modname == "logging" {
                    let bound = match as_name {
                        Some(a) => md.tree.s(a).to_string(),
                        None => modname,
                    };
                    self.logging_names.insert(bound);
                }
            }
        }
    }

    /// visit_call (logging.py:191-220)
    pub fn visit_call(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let Some((fnode, _args, _)) = crate::typecheck::call_parts(eng, node) else { return };
        // is_logging_name(): Attribute(expr=Name(name in logging_names))
        let mut name: Option<String> = None;
        {
            let md = eng.md(fnode.m);
            if let NodeKind::Attribute { expr, attrname, .. } = &md.tree.nodes[fnode.n.idx()].kind {
                if let NodeKind::Name { name: n } = &md.tree.nodes[expr.idx()].kind {
                    if self.logging_names.contains(md.tree.s(*n)) {
                        name = Some(md.tree.s(*attrname).to_string());
                    }
                }
            }
        }
        if name.is_none() {
            // is_logger_class(): infer_all(node.func) -> BoundMethod owned
            // by logging.Logger (or a subclass)
            let vals = u::infer_all(eng, cx.caches, fnode);
            for v in vals.iter() {
                if let Value::BoundMethod { func, .. } = v {
                    let Some(parent) = eng.parent(*func) else { continue };
                    if !is_classdef(eng, parent) {
                        continue;
                    }
                    let q = eng.value_qname(&Value::Node(parent));
                    let is_logger = q.as_deref() == Some("logging.Logger")
                        || eng.ancestors(parent, true, None).iter().any(|&a| {
                            eng.value_qname(&Value::Node(a)).as_deref()
                                == Some("logging.Logger")
                        });
                    if is_logger {
                        name = eng.node_name(*func);
                        break;
                    }
                }
            }
        }
        let Some(name) = name else { return };
        self.check_log_method(cx, node, &name);
    }

    /// _check_log_method (logging.py:222-269)
    fn check_log_method(&mut self, cx: &mut WalkCx, node: GNode, name: &str) {
        let eng = cx.eng;
        let Some((_, args, keywords)) = crate::typecheck::call_parts(eng, node) else { return };
        let has_star = args
            .iter()
            .any(|&a| eng.kind_is(a, |k| matches!(k, NodeKind::Starred { .. })));
        let has_kwargs = keywords.iter().any(|&k| {
            let md = eng.md(k.m);
            matches!(&md.tree.nodes[k.n.idx()].kind, NodeKind::Keyword { arg: None, .. })
        });
        let format_pos: usize;
        if name == "log" {
            if has_star || has_kwargs || args.len() < 2 {
                return;
            }
            format_pos = 1;
        } else if CHECKED_CONVENIENCE_FUNCTIONS.contains(&name) {
            if has_star || has_kwargs || args.is_empty() {
                return;
            }
            format_pos = 0;
        } else {
            return;
        }
        let format_arg = args[format_pos];
        let md = eng.md(format_arg.m);
        match &md.tree.nodes[format_arg.n.idx()].kind {
            NodeKind::BinOp { op, left, right } => {
                let op = op.to_string();
                let (left, right) = (
                    GNode { m: format_arg.m, n: *left },
                    GNode { m: format_arg.m, n: *right },
                );
                drop(md);
                let mut emit = op == "%";
                if op == "+" && !is_node_explicit_str_concatenation(eng, format_arg) {
                    let total = [left, right]
                        .iter()
                        .filter(|&&o| {
                            is_operand_literal_str_v(
                                eng,
                                &u::safe_infer(eng, cx.caches, o),
                            )
                        })
                        .count();
                    emit = total > 0;
                }
                if emit {
                    let hs = helper_string(cx, node);
                    cx.emit_node(
                        "W1201",
                        u::lineno(eng, node),
                        u::col_offset(eng, node) as i64,
                        u::format_template("Use %s formatting in logging functions", &[&hs]),
                    );
                }
            }
            NodeKind::Call { .. } => {
                drop(md);
                self.check_call_func(cx, node, format_arg);
            }
            NodeKind::Const(_) => {
                drop(md);
                self.check_format_string(cx, node, &args, format_pos);
            }
            NodeKind::JoinedStr { values } => {
                // str_formatting_in_f_string
                let mut has_pct = false;
                for &v in values {
                    if let NodeKind::Const(ConstValue::Str(s)) = &md.tree.nodes[v.idx()].kind {
                        if s.contains('%')
                            && MOST_COMMON_FORMATTING.iter().any(|f| s.contains(f))
                        {
                            has_pct = true;
                            break;
                        }
                    }
                }
                drop(md);
                if has_pct {
                    return;
                }
                let hs = helper_string(cx, node);
                cx.emit_node(
                    "W1203",
                    u::lineno(eng, node),
                    u::col_offset(eng, node) as i64,
                    u::format_template("Use %s formatting in logging functions", &[&hs]),
                );
            }
            _ => {}
        }
    }

    /// _check_call_func — W1202
    fn check_call_func(&mut self, cx: &mut WalkCx, lognode: GNode, call: GNode) {
        let eng = cx.eng;
        let Some((fnode, _, _)) = crate::typecheck::call_parts(eng, call) else { return };
        let func = u::safe_infer(eng, cx.caches, fnode);
        let Some(Value::BoundMethod { func: meth, bound }) = func else { return };
        // is_method_call(func, ("str","unicode"), ("format",))
        if !crate::typecheck::value_is_instance(eng, &bound) {
            return;
        }
        let bname = crate::typecheck::value_name(eng, &bound).unwrap_or_default();
        if bname != "str" && bname != "unicode" {
            return;
        }
        if eng.node_name(meth).as_deref() != Some("format") {
            return;
        }
        // is_complex_format_str(func.bound)
        if is_complex_format_str(eng, cx, &bound) {
            return;
        }
        let hs = helper_string(cx, lognode);
        cx.emit_node(
            "W1202",
            u::lineno(eng, lognode),
            u::col_offset(eng, lognode) as i64,
            u::format_template("Use %s formatting in logging functions", &[&hs]),
        );
    }

    /// _check_format_string (logging.py:322-375)
    fn check_format_string(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        args: &[GNode],
        format_pos: usize,
    ) {
        let eng = cx.eng;
        // _count_supplied_tokens: post-format args that are not Keyword
        // (keywords are not in args in our tree, so all count)
        let num_args = args.len().saturating_sub(format_pos + 1);
        let format_string: Option<String> = {
            let md = eng.md(args[format_pos].m);
            match &md.tree.nodes[args[format_pos].n.idx()].kind {
                NodeKind::Const(ConstValue::Str(s)) => Some(s.to_string()),
                NodeKind::Const(ConstValue::Bytes(b)) => {
                    // format_string.decode() — STRICT utf-8 (logging.py:333).
                    // A non-decodable bytes literal raises UnicodeDecodeError
                    // in pylint, aborting the whole module check -> F0002
                    // (pip tests/unit/test_base_command.py:62 b"... \xe9")
                    match std::str::from_utf8(b) {
                        Ok(s) => Some(s.to_string()),
                        Err(_) => {
                            cx.crashed.set(true);
                            return;
                        }
                    }
                }
                _ => None,
            }
        };
        let mut required_num_args = 0usize;
        if let Some(fs) = format_string {
            match parse_format_string(&fs) {
                Ok(parsed) => {
                    if !parsed.keys.is_empty() {
                        return; // named specifiers -> out of scope
                    }
                    required_num_args = parsed.num_args;
                }
                Err(PctError::Unsupported(index)) => {
                    if num_args > 0 {
                        let ch: char = fs.chars().nth(index).unwrap_or('?');
                        cx.emit_node(
                            "E1200",
                            u::lineno(eng, node),
                            u::col_offset(eng, node) as i64,
                            format!(
                                "Unsupported logging format character {} ({:#02x}) at index {}",
                                u::py_repr_str(&ch.to_string()),
                                ch as u32,
                                index
                            ),
                        );
                    }
                    return;
                }
                Err(PctError::Incomplete) => {
                    cx.emit_node(
                        "E1201",
                        u::lineno(eng, node),
                        u::col_offset(eng, node) as i64,
                        "Logging format string ends in middle of conversion specifier".into(),
                    );
                    return;
                }
            }
        }
        if num_args > required_num_args {
            cx.emit_node(
                "E1205",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                "Too many arguments for logging format string".into(),
            );
        } else if num_args < required_num_args {
            cx.emit_node(
                "E1206",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                "Not enough arguments for logging format string".into(),
            );
        }
    }
}

/// _helper_string (logging.py:271-286)
fn helper_string(cx: &WalkCx, node: GNode) -> String {
    let line = u::lineno(cx.eng, node);
    let mut valid = vec!["lazy %"];
    // "logging-fstring-formatting" is an UNKNOWN msgid — pylint's
    // is_message_enabled returns True for unknown ids (msgids_to_check
    // lookup falls through) -> NOT appended. Verified: pylint tolerates
    // the unknown id and the enabled-check yields True.
    if !(cx.is_enabled)("W1202", line) {
        valid.push(".format()");
    }
    if !(cx.is_enabled)("W1201", line) {
        valid.push("%");
    }
    valid.join(" or ")
}

fn is_operand_literal_str_v(eng: &Engine, v: &Option<Value>) -> bool {
    match v {
        Some(Value::Node(g)) => {
            let md = eng.md(g.m);
            matches!(
                md.tree.nodes[g.n.idx()].kind,
                NodeKind::Const(ConstValue::Str(_))
            )
        }
        Some(Value::SynthConst(c)) => matches!(&**c, ConstValue::Str(_)),
        _ => false,
    }
}

fn is_operand_literal_str_node(eng: &Engine, g: GNode) -> bool {
    let md = eng.md(g.m);
    matches!(
        md.tree.nodes[g.n.idx()].kind,
        NodeKind::Const(ConstValue::Str(_))
    )
}

fn is_node_explicit_str_concatenation(eng: &Engine, node: GNode) -> bool {
    let md = eng.md(node.m);
    let NodeKind::BinOp { left, right, .. } = &md.tree.nodes[node.n.idx()].kind else {
        return false;
    };
    let (l, r) = (GNode { m: node.m, n: *left }, GNode { m: node.m, n: *right });
    drop(md);
    (is_operand_literal_str_node(eng, l) || is_node_explicit_str_concatenation(eng, l))
        && (is_operand_literal_str_node(eng, r) || is_node_explicit_str_concatenation(eng, r))
}

/// is_complex_format_str (logging.py:378-388)
fn is_complex_format_str(eng: &Engine, cx: &WalkCx, bound: &Value) -> bool {
    // safe_infer(node) where node is the bound value
    let inferred: Option<Value> = match bound {
        Value::Node(g) => u::safe_infer(eng, cx.caches, *g),
        other => Some(other.clone()),
    };
    let s: Option<String> = match &inferred {
        Some(Value::Node(g)) => {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::Const(ConstValue::Str(s)) => Some(s.to_string()),
                _ => None,
            }
        }
        Some(Value::SynthConst(c)) => match &**c {
            ConstValue::Str(s) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    };
    let Some(s) = s else { return true };
    match crate::strings::formatter_parse(&s) {
        Err(()) => false,
        Ok(entries) => entries
            .iter()
            .any(|e| e.format_spec.as_deref().map(|x| !x.is_empty()).unwrap_or(false)),
    }
}
