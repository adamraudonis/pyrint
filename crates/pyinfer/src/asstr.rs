//! Faithful port of astroid's AsStringVisitor (astroid/nodes/as_string.py,
//! 4.0.4) over the pyast tree. Bug-for-bug: NamedExpr renders bare
//! (`t := v`, never parenthesized — as_string.py:441-445), which makes
//! re-parses of rendered code FAIL for walruses in paren-requiring spots
//! (the dataclass-__init__ generation depends on that failure).

use pyast::tree::{ConstValue, IntValue, NodeKind};
use pyast::NodeId;

use crate::graph::Engine;
use crate::value::GNode;

const DOC_NEWLINE: char = '\0';

/// OP_PRECEDENCE (astroid/nodes/const.py:5-26); default = len(table) = 21
fn op_prec_of(op: &str) -> Option<u32> {
    Some(match op {
        "or" => 2,
        "and" => 3,
        "not" => 4,
        "|" => 6,
        "^" => 7,
        "&" => 8,
        "<<" | ">>" => 9,
        "+" | "-" => 10,
        "*" | "@" | "/" | "//" | "%" => 11,
        "**" => 13,
        _ => return None,
    })
}
const DEFAULT_PREC: u32 = 21;

pub struct AsStr<'e> {
    pub eng: &'e Engine,
    pub indent: &'static str,
}

pub fn as_string(eng: &Engine, g: GNode) -> String {
    AsStr { eng, indent: "    " }
        .accept(g)
        .replace(DOC_NEWLINE, "\n")
}

impl<'e> AsStr<'e> {
    fn k(&self, g: GNode) -> std::rc::Rc<crate::graph::Module> {
        self.eng.md(g.m)
    }
    fn child(&self, g: GNode, n: NodeId) -> GNode {
        GNode { m: g.m, n }
    }

    /// node.op_precedence()
    fn op_precedence(&self, g: GNode) -> u32 {
        let md = self.k(g);
        match &md.tree.nodes[g.n.idx()].kind {
            NodeKind::Lambda(_) => 0,
            NodeKind::IfExp { .. } => 1,
            NodeKind::BoolOp { op, .. } => op_prec_of(op).unwrap_or(DEFAULT_PREC),
            NodeKind::Compare { .. } => 5,
            NodeKind::BinOp { op, .. } => op_prec_of(op).unwrap_or(DEFAULT_PREC),
            NodeKind::UnaryOp { op, .. } => {
                if &**op == "not" {
                    4
                } else {
                    12
                }
            }
            NodeKind::Await { .. } => 14,
            _ => DEFAULT_PREC,
        }
    }

    /// node.op_left_associative(): False only for `**` BinOp and IfExp
    fn op_left_associative(&self, g: GNode) -> bool {
        let md = self.k(g);
        match &md.tree.nodes[g.n.idx()].kind {
            NodeKind::BinOp { op, .. } => &**op != "**",
            NodeKind::IfExp { .. } => false,
            _ => true,
        }
    }

    fn should_wrap(&self, node: GNode, child: GNode, is_left: bool) -> bool {
        let np = self.op_precedence(node);
        let cp = self.op_precedence(child);
        if np > cp {
            return true;
        }
        np == cp && is_left != self.op_left_associative(node)
    }

    fn prec_parens(&self, node: GNode, child: GNode, is_left: bool) -> String {
        if self.should_wrap(node, child, is_left) {
            format!("({})", self.accept(child))
        } else {
            self.accept(child)
        }
    }

    fn stmt_list(&self, g: GNode, stmts: &[NodeId], indent: bool) -> String {
        let strs: Vec<String> = stmts
            .iter()
            .map(|&s| self.accept(self.child(g, s)))
            .filter(|s| !s.is_empty())
            .collect();
        let joined = strs.join("\n");
        if !indent {
            return joined;
        }
        format!("{}{}", self.indent, joined.replace('\n', &format!("\n{}", self.indent)))
    }

    fn docs_dedent(&self, g: GNode, doc: Option<NodeId>) -> String {
        let Some(d) = doc else { return String::new() };
        let md = self.k(g);
        let text = match &md.tree.nodes[d.idx()].kind {
            NodeKind::Const(ConstValue::Str(s)) => s.to_string(),
            _ => return String::new(),
        };
        format!(
            "\n{}\"\"\"{}\"\"\"",
            self.indent,
            text.replace('\n', &DOC_NEWLINE.to_string())
        )
    }

    fn opt(&self, g: GNode, n: Option<NodeId>) -> String {
        n.map(|x| self.accept(self.child(g, x))).unwrap_or_default()
    }

    pub fn accept(&self, g: GNode) -> String {
        let md = self.k(g);
        let s = |n: &NodeId| self.accept(self.child(g, *n));
        let sym = |x: &pyast::tree::Sym| md.tree.s(*x).to_string();
        match &md.tree.nodes[g.n.idx()].kind {
            NodeKind::Module(d) => {
                let docs = match d.doc_node {
                    Some(dn) => match &md.tree.nodes[dn.idx()].kind {
                        NodeKind::Const(ConstValue::Str(t)) => format!("\"\"\"{t}\"\"\"\n\n"),
                        _ => String::new(),
                    },
                    None => String::new(),
                };
                let body: Vec<String> = d.body.iter().map(s).collect();
                format!("{}{}\n\n", docs, body.join("\n"))
            }
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                let keyword = if matches!(md.tree.nodes[g.n.idx()].kind, NodeKind::AsyncFunctionDef(_)) {
                    "async def"
                } else {
                    "def"
                };
                let decorate = self.opt(g, d.decorators);
                let docs = self.docs_dedent(g, d.doc_node);
                let trailer = match d.returns {
                    Some(r) => format!(" -> {}:", s(&r)),
                    None => ":".to_string(),
                };
                let tp = self.type_params(g, &d.type_params);
                format!(
                    "\n{}{} {}{}({}){}{}\n{}",
                    decorate,
                    keyword,
                    sym(&d.name),
                    tp,
                    s(&d.args),
                    trailer,
                    docs,
                    self.stmt_list(g, &d.body, true)
                )
            }
            NodeKind::ClassDef(d) => {
                let decorate = self.opt(g, d.decorators);
                let tp = self.type_params(g, &d.type_params);
                let mut args: Vec<String> = d.bases.iter().map(s).collect();
                // node._metaclass and not node.has_metaclass_hack()
                if let Some(mc) = d.metaclass {
                    args.push(format!("metaclass={}", s(&mc)));
                }
                args.extend(d.keywords.iter().map(s));
                let args_str = if args.is_empty() {
                    String::new()
                } else {
                    format!("({})", args.join(", "))
                };
                let docs = self.docs_dedent(g, d.doc_node);
                format!(
                    "\n\n{}class {}{}{}:{}\n{}\n",
                    decorate,
                    sym(&d.name),
                    tp,
                    args_str,
                    docs,
                    self.stmt_list(g, &d.body, true)
                )
            }
            NodeKind::Return { value } => match value {
                Some(v) => {
                    // is_tuple_return with >1 elts: bare commas
                    if let NodeKind::Tuple { elts, .. } = &md.tree.nodes[v.idx()].kind {
                        if elts.len() > 1 {
                            let parts: Vec<String> = elts.iter().map(s).collect();
                            return format!("return {}", parts.join(", "));
                        }
                    }
                    format!("return {}", s(v))
                }
                None => "return".to_string(),
            },
            NodeKind::Delete { targets } => {
                let parts: Vec<String> = targets.iter().map(s).collect();
                format!("del {}", parts.join(", "))
            }
            NodeKind::Assign { targets, value } => {
                let lhs: Vec<String> = targets.iter().map(s).collect();
                format!("{} = {}", lhs.join(" = "), s(value))
            }
            NodeKind::AugAssign { target, op, value } => {
                format!("{} {} {}", s(target), op, s(value))
            }
            NodeKind::AnnAssign { target, annotation, value, .. } => match value {
                Some(v) => format!("{}: {} = {}", s(target), s(annotation), s(v)),
                None => format!("{}: {}", s(target), s(annotation)),
            },
            NodeKind::TypeAlias { name, type_params, value } => {
                format!(
                    "type {}{} = {}",
                    s(name),
                    self.type_params(g, type_params),
                    s(value)
                )
            }
            NodeKind::For(d) | NodeKind::AsyncFor(d) => {
                let prefix = if matches!(md.tree.nodes[g.n.idx()].kind, NodeKind::AsyncFor(_)) {
                    "async "
                } else {
                    ""
                };
                let mut fors = format!(
                    "{}for {} in {}:\n{}",
                    prefix,
                    s(&d.target),
                    s(&d.iter),
                    self.stmt_list(g, &d.body, true)
                );
                if !d.orelse.is_empty() {
                    fors = format!("{}\nelse:\n{}", fors, self.stmt_list(g, &d.orelse, true));
                }
                fors
            }
            NodeKind::While { test, body, orelse } => {
                let mut out = format!("while {}:\n{}", s(test), self.stmt_list(g, body, true));
                if !orelse.is_empty() {
                    out = format!("{}\nelse:\n{}", out, self.stmt_list(g, orelse, true));
                }
                out
            }
            NodeKind::If { test, body, orelse } => {
                let mut ifs = vec![format!("if {}:\n{}", s(test), self.stmt_list(g, body, true))];
                // has_elif_block: single-If orelse
                let is_elif = orelse.len() == 1
                    && matches!(md.tree.nodes[orelse[0].idx()].kind, NodeKind::If { .. });
                if is_elif {
                    ifs.push(format!("el{}", self.stmt_list(g, orelse, false)));
                } else if !orelse.is_empty() {
                    ifs.push(format!("else:\n{}", self.stmt_list(g, orelse, true)));
                }
                ifs.join("\n")
            }
            NodeKind::With(d) | NodeKind::AsyncWith(d) => {
                let prefix = if matches!(md.tree.nodes[g.n.idx()].kind, NodeKind::AsyncWith(_)) {
                    "async "
                } else {
                    ""
                };
                let items: Vec<String> = d
                    .items
                    .iter()
                    .map(|(expr, v)| match v {
                        Some(vn) => format!("{} as {}", s(expr), s(vn)),
                        None => s(expr),
                    })
                    .collect();
                format!(
                    "{}with {}:\n{}",
                    prefix,
                    items.join(", "),
                    self.stmt_list(g, &d.body, true)
                )
            }
            NodeKind::Match { subject, cases } => {
                format!("match {}:\n{}", s(subject), self.stmt_list(g, cases, true))
            }
            NodeKind::Raise { exc, cause } => match (exc, cause) {
                (Some(e), Some(c)) => format!("raise {} from {}", s(e), s(c)),
                (Some(e), None) => format!("raise {}", s(e)),
                _ => "raise".to_string(),
            },
            NodeKind::Try(d) | NodeKind::TryStar(d) => {
                let mut trys = vec![format!("try:\n{}", self.stmt_list(g, &d.body, true))];
                for h in &d.handlers {
                    trys.push(s(h));
                }
                if !d.orelse.is_empty() {
                    trys.push(format!("else:\n{}", self.stmt_list(g, &d.orelse, true)));
                }
                if !d.finalbody.is_empty() {
                    trys.push(format!("finally:\n{}", self.stmt_list(g, &d.finalbody, true)));
                }
                trys.join("\n")
            }
            NodeKind::Assert { test, fail } => match fail {
                Some(f) => format!("assert {}, {}", s(test), s(f)),
                None => format!("assert {}", s(test)),
            },
            NodeKind::Import { names } => format!("import {}", import_string(&md, names)),
            NodeKind::ImportFrom { modname, names, level } => {
                format!(
                    "from {}{} import {}",
                    ".".repeat(level.unwrap_or(0) as usize),
                    md.tree.s(*modname),
                    import_string(&md, names)
                )
            }
            NodeKind::Global { names } => {
                let parts: Vec<String> = names.iter().map(|n| md.tree.s(*n).to_string()).collect();
                format!("global {}", parts.join(", "))
            }
            NodeKind::Nonlocal { names } => {
                let parts: Vec<String> = names.iter().map(|n| md.tree.s(*n).to_string()).collect();
                format!("nonlocal {}", parts.join(", "))
            }
            NodeKind::Expr { value } => s(value),
            NodeKind::Pass => "pass".to_string(),
            NodeKind::Break => "break".to_string(),
            NodeKind::Continue => "continue".to_string(),
            NodeKind::MatchCase { pattern, guard, body } => {
                let guard_str = match guard {
                    Some(gd) => format!(" if {}", s(gd)),
                    None => String::new(),
                };
                format!(
                    "case {}{}:\n{}",
                    s(pattern),
                    guard_str,
                    self.stmt_list(g, body, true)
                )
            }
            NodeKind::MatchValue { value } => s(value),
            NodeKind::MatchSingleton { value } => const_repr(value),
            NodeKind::MatchSequence { patterns } => {
                let parts: Vec<String> = patterns.iter().map(s).collect();
                format!("[{}]", parts.join(", "))
            }
            NodeKind::MatchMapping { keys, patterns, rest } => {
                let mut parts: Vec<String> = keys
                    .iter()
                    .zip(patterns.iter())
                    .map(|(k, p)| format!("{}: {}", s(k), s(p)))
                    .collect();
                if let Some(r) = rest {
                    parts.push(format!("**{}", s(r)));
                }
                format!("{{{}}}", parts.join(", "))
            }
            NodeKind::MatchClass { cls, patterns, kwd_attrs, kwd_patterns } => {
                let mut parts: Vec<String> = patterns.iter().map(s).collect();
                for (a, p) in kwd_attrs.iter().zip(kwd_patterns.iter()) {
                    parts.push(format!("{}={}", md.tree.s(*a), s(p)));
                }
                format!("{}({})", s(cls), parts.join(", "))
            }
            NodeKind::MatchStar { name } => match name {
                Some(n) => format!("*{}", s(n)),
                None => "*_".to_string(),
            },
            NodeKind::MatchAs { pattern, name } => {
                // parent MatchSequence/MatchMapping/MatchClass -> bare name
                let parent_kind_seq = self
                    .eng
                    .parent(g)
                    .map(|p| {
                        self.eng.kind_is(p, |k| {
                            matches!(
                                k,
                                NodeKind::MatchSequence { .. }
                                    | NodeKind::MatchMapping { .. }
                                    | NodeKind::MatchClass { .. }
                            )
                        })
                    })
                    .unwrap_or(false);
                if parent_kind_seq {
                    return match name {
                        Some(n) => s(n),
                        None => "_".to_string(),
                    };
                }
                let pat = match pattern {
                    Some(p) => s(p),
                    None => "_".to_string(),
                };
                match name {
                    Some(n) => format!("{} as {}", pat, s(n)),
                    None => pat,
                }
            }
            NodeKind::MatchOr { patterns } => {
                let parts: Vec<String> = patterns.iter().map(s).collect();
                parts.join(" | ")
            }
            NodeKind::BoolOp { op, values } => {
                let parts: Vec<String> = values
                    .iter()
                    .map(|&v| self.prec_parens(g, self.child(g, v), true))
                    .collect();
                parts.join(&format!(" {op} "))
            }
            NodeKind::NamedExpr { target, value } => {
                // as_string.py:441-445 — ALWAYS bare
                format!("{} := {}", s(target), s(value))
            }
            NodeKind::BinOp { left, op, right } => {
                let l = self.prec_parens(g, self.child(g, *left), true);
                let r = self.prec_parens(g, self.child(g, *right), false);
                if &**op == "**" {
                    format!("{l}{op}{r}")
                } else {
                    format!("{l} {op} {r}")
                }
            }
            NodeKind::UnaryOp { op, operand } => {
                let operator = if &**op == "not" { "not ".to_string() } else { op.to_string() };
                format!("{}{}", operator, self.prec_parens(g, self.child(g, *operand), true))
            }
            NodeKind::Lambda(d) => {
                let args = s(&d.args);
                let body = s(&d.body);
                if !args.is_empty() {
                    format!("lambda {args}: {body}")
                } else {
                    format!("lambda: {body}")
                }
            }
            NodeKind::IfExp { test, body, orelse } => {
                format!(
                    "{} if {} else {}",
                    self.prec_parens(g, self.child(g, *body), true),
                    self.prec_parens(g, self.child(g, *test), true),
                    self.prec_parens(g, self.child(g, *orelse), false)
                )
            }
            NodeKind::Dict { items } => {
                let parts: Vec<String> = items
                    .iter()
                    .map(|(k, v)| {
                        let ks = s(k);
                        let vs = s(v);
                        if ks == "**" {
                            format!("{ks}{vs}")
                        } else {
                            format!("{ks}: {vs}")
                        }
                    })
                    .collect();
                format!("{{{}}}", parts.join(", "))
            }
            NodeKind::DictUnpack => "**".to_string(),
            NodeKind::Set { elts } => {
                let parts: Vec<String> = elts.iter().map(s).collect();
                format!("{{{}}}", parts.join(", "))
            }
            NodeKind::ListComp(d) => {
                let gens: Vec<String> = d.generators.iter().map(s).collect();
                format!("[{} {}]", s(&d.elt), gens.join(" "))
            }
            NodeKind::SetComp(d) => {
                let gens: Vec<String> = d.generators.iter().map(s).collect();
                format!("{{{} {}}}", s(&d.elt), gens.join(" "))
            }
            NodeKind::DictComp(d) => {
                let gens: Vec<String> = d.generators.iter().map(s).collect();
                format!("{{{}: {} {}}}", s(&d.key), s(&d.value), gens.join(" "))
            }
            NodeKind::GeneratorExp(d) => {
                let gens: Vec<String> = d.generators.iter().map(s).collect();
                format!("({} {})", s(&d.elt), gens.join(" "))
            }
            NodeKind::Await { value } => format!("await {}", s(value)),
            NodeKind::Yield { value } => {
                let yi = match value {
                    Some(v) => format!(" {}", s(v)),
                    None => String::new(),
                };
                let expr = format!("yield{yi}");
                if self.parent_is_statement(g) {
                    expr
                } else {
                    format!("({expr})")
                }
            }
            NodeKind::YieldFrom { value } => {
                let expr = format!("yield from {}", s(value));
                if self.parent_is_statement(g) {
                    expr
                } else {
                    format!("({expr})")
                }
            }
            NodeKind::Compare { left, ops } => {
                let rhs: Vec<String> = ops
                    .iter()
                    .map(|(op, expr)| {
                        format!("{} {}", op, self.prec_parens(g, self.child(g, *expr), false))
                    })
                    .collect();
                format!(
                    "{} {}",
                    self.prec_parens(g, self.child(g, *left), true),
                    rhs.join(" ")
                )
            }
            NodeKind::Call { func, args, keywords } => {
                let expr_str = self.prec_parens(g, self.child(g, *func), true);
                let mut parts: Vec<String> = args.iter().map(s).collect();
                parts.extend(keywords.iter().map(s));
                format!("{}({})", expr_str, parts.join(", "))
            }
            NodeKind::FormattedValue { value, conversion, format_spec } => {
                let mut result = s(value);
                if *conversion >= 0 {
                    result.push('!');
                    result.push(char::from_u32(*conversion as u32).unwrap_or('r'));
                }
                if let Some(spec) = format_spec {
                    let spec_str = s(spec);
                    // strip the leading f and quotes
                    let trimmed = spec_str
                        .strip_prefix('f')
                        .map(|x| {
                            let mut chars = x.chars();
                            chars.next();
                            let mut t = chars.as_str().to_string();
                            t.pop();
                            t
                        })
                        .unwrap_or_default();
                    result.push(':');
                    result.push_str(&trimmed);
                }
                format!("{{{result}}}")
            }
            NodeKind::JoinedStr { values } => {
                let mut string = String::new();
                for &v in values {
                    let vg = self.child(g, v);
                    let vmd = self.k(vg);
                    match &vmd.tree.nodes[v.idx()].kind {
                        NodeKind::Const(ConstValue::Str(t)) => {
                            let r = pyast::pyrepr::repr_str(t);
                            let inner = &r[1..r.len() - 1];
                            string.push_str(&inner.replace('{', "{{").replace('}', "}}"));
                        }
                        NodeKind::Const(c) => {
                            let r = const_repr(c);
                            if r.len() >= 2 {
                                string.push_str(
                                    &r[1..r.len() - 1].replace('{', "{{").replace('}', "}}"),
                                );
                            }
                        }
                        _ => string.push_str(&self.accept(vg)),
                    }
                }
                let mut quote = "'";
                for q in ["'", "\"", "\"\"\"", "'''"] {
                    if !string.contains(q) {
                        quote = q;
                        break;
                    }
                }
                format!("f{quote}{string}{quote}")
            }
            NodeKind::Const(c) => const_repr(c),
            NodeKind::Starred { value, .. } => format!("*{}", s(value)),
            NodeKind::Attribute { expr, attrname, .. }
            | NodeKind::AssignAttr { expr, attrname }
            | NodeKind::DelAttr { expr, attrname } => {
                let mut left = self.prec_parens(g, self.child(g, *expr), true);
                if !left.is_empty() && left.chars().all(|c| c.is_ascii_digit()) {
                    left = format!("({left})");
                }
                format!("{}.{}", left, md.tree.s(*attrname))
            }
            NodeKind::Subscript { value, slice, .. } => {
                let idx = self.child(g, *slice);
                let mut idxstr = self.accept(idx);
                let imd = self.k(idx);
                if let NodeKind::Tuple { elts, .. } = &imd.tree.nodes[slice.idx()].kind {
                    if !elts.is_empty() {
                        // strip tuple parens: a[(::1, 1:)] is invalid
                        idxstr = idxstr[1..idxstr.len() - 1].to_string();
                    }
                }
                format!(
                    "{}[{}]",
                    self.prec_parens(g, self.child(g, *value), true),
                    idxstr
                )
            }
            NodeKind::Name { name } | NodeKind::AssignName { name } | NodeKind::DelName { name } => {
                md.tree.s(*name).to_string()
            }
            NodeKind::List { elts, .. } => {
                let parts: Vec<String> = elts.iter().map(s).collect();
                format!("[{}]", parts.join(", "))
            }
            NodeKind::Tuple { elts, .. } => {
                if elts.len() == 1 {
                    format!("({}, )", s(&elts[0]))
                } else {
                    let parts: Vec<String> = elts.iter().map(s).collect();
                    format!("({})", parts.join(", "))
                }
            }
            NodeKind::Slice { lower, upper, step } => {
                let l = self.opt(g, *lower);
                let u = self.opt(g, *upper);
                let st = self.opt(g, *step);
                if !st.is_empty() {
                    format!("{l}:{u}:{st}")
                } else {
                    format!("{l}:{u}")
                }
            }
            NodeKind::Comprehension { target, iter, ifs, is_async } => {
                let ifs_str: String = ifs.iter().map(|&i| format!(" if {}", s(&i))).collect();
                let generated = format!("for {} in {}{}", s(target), s(iter), ifs_str);
                format!("{}{}", if *is_async { "async " } else { "" }, generated)
            }
            NodeKind::ExceptHandler { type_, name, body } => {
                let n = if self
                    .eng
                    .parent(g)
                    .map(|p| self.eng.kind_is(p, |k| matches!(k, NodeKind::TryStar(_))))
                    .unwrap_or(false)
                {
                    "except*"
                } else {
                    "except"
                };
                let excs = match (type_, name) {
                    (Some(t), Some(nm)) => format!("{} {} as {}", n, s(t), s(nm)),
                    (Some(t), None) => format!("{} {}", n, s(t)),
                    _ => n.to_string(),
                };
                format!("{}:\n{}", excs, self.stmt_list(g, body, true))
            }
            NodeKind::Arguments(_) => self.format_args(g),
            NodeKind::Keyword { arg, value } => match arg {
                Some(a) => format!("{}={}", md.tree.s(*a), s(value)),
                None => format!("**{}", s(value)),
            },
            NodeKind::Decorators { nodes } => {
                let parts: Vec<String> = nodes.iter().map(s).collect();
                format!("@{}\n", parts.join("\n@"))
            }
            NodeKind::TypeVar { name, bound } => {
                let b = match bound {
                    Some(bn) => format!(": {}", s(bn)),
                    None => String::new(),
                };
                format!("{}{}", s(name), b)
            }
            NodeKind::ParamSpec { name } => format!("**{}", s(name)),
            NodeKind::TypeVarTuple { name } => format!("*{}", s(name)),
            NodeKind::EmptyNode | NodeKind::Unknown => String::new(),
        }
    }

    fn type_params(&self, g: GNode, tp: &[NodeId]) -> String {
        if tp.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = tp.iter().map(|&t| self.accept(self.child(g, t))).collect();
        format!("[{}]", parts.join(", "))
    }

    fn parent_is_statement(&self, g: GNode) -> bool {
        let Some(p) = self.eng.parent(g) else { return false };
        self.eng.kind_is(p, |k| {
            matches!(
                k,
                NodeKind::Expr { .. }
                    | NodeKind::Assign { .. }
                    | NodeKind::AugAssign { .. }
                    | NodeKind::AnnAssign { .. }
                    | NodeKind::Return { .. }
                    | NodeKind::Delete { .. }
                    | NodeKind::For(_)
                    | NodeKind::AsyncFor(_)
                    | NodeKind::While { .. }
                    | NodeKind::If { .. }
                    | NodeKind::With(_)
                    | NodeKind::AsyncWith(_)
                    | NodeKind::Raise { .. }
                    | NodeKind::Try(_)
                    | NodeKind::TryStar(_)
                    | NodeKind::Assert { .. }
                    | NodeKind::Import { .. }
                    | NodeKind::ImportFrom { .. }
                    | NodeKind::Global { .. }
                    | NodeKind::Nonlocal { .. }
                    | NodeKind::Pass
                    | NodeKind::Break
                    | NodeKind::Continue
                    | NodeKind::FunctionDef(_)
                    | NodeKind::AsyncFunctionDef(_)
                    | NodeKind::ClassDef(_)
                    | NodeKind::TypeAlias { .. }
                    | NodeKind::Match { .. }
                    | NodeKind::MatchCase { .. }
            )
        })
    }

    /// Arguments.format_args (node_classes.py:811-859)
    fn format_args(&self, g: GNode) -> String {
        let md = self.k(g);
        let NodeKind::Arguments(a) = &md.tree.nodes[g.n.idx()].kind else {
            return String::new();
        };
        let mut result: Vec<String> = Vec::new();
        let n_defaults = a.defaults.len();
        let n_args = a.args.len();
        let (pos_only_defaults, pos_kw_defaults): (Vec<Option<NodeId>>, Vec<Option<NodeId>>) =
            if n_defaults > 0 {
                let split = n_defaults.saturating_sub(n_args);
                (
                    a.defaults[..split].iter().map(|&d| Some(d)).collect(),
                    a.defaults[split..].iter().map(|&d| Some(d)).collect(),
                )
            } else {
                (Vec::new(), a.defaults.iter().map(|&d| Some(d)).collect())
            };
        if !a.posonlyargs.is_empty() {
            result.push(self.format_args_list(
                g,
                &a.posonlyargs,
                Some(&pos_only_defaults),
                &a.posonlyargs_annotations,
            ));
            result.push("/".to_string());
        }
        if !a.args.is_empty() {
            result.push(self.format_args_list(
                g,
                &a.args,
                Some(&pos_kw_defaults),
                &a.annotations,
            ));
        }
        if let Some(v) = a.vararg {
            result.push(format!("*{}", md.tree.s(v)));
        }
        if !a.kwonlyargs.is_empty() {
            if a.vararg.is_none() {
                result.push("*".to_string());
            }
            result.push(self.format_args_list(
                g,
                &a.kwonlyargs,
                Some(&a.kw_defaults),
                &a.kwonlyargs_annotations,
            ));
        }
        if let Some(kw) = a.kwarg {
            result.push(format!("**{}", md.tree.s(kw)));
        }
        result.join(", ")
    }

    /// node_classes._format_args (node_classes.py:1042-1073)
    fn format_args_list(
        &self,
        g: GNode,
        args: &[NodeId],
        defaults: Option<&[Option<NodeId>]>,
        annotations: &[Option<NodeId>],
    ) -> String {
        let md = self.k(g);
        let mut values: Vec<String> = Vec::new();
        let default_offset = defaults.map(|d| args.len().saturating_sub(d.len()));
        for (i, &arg) in args.iter().enumerate() {
            let argname = match &md.tree.nodes[arg.idx()].kind {
                NodeKind::AssignName { name } => md.tree.s(*name).to_string(),
                _ => String::new(),
            };
            let annotation = annotations.get(i).copied().flatten();
            let (mut value, default_sep) = match annotation {
                Some(ann) => (
                    format!("{}: {}", argname, self.accept(self.child(g, ann))),
                    " = ",
                ),
                None => (argname, "="),
            };
            if let (Some(offset), Some(defs)) = (default_offset, defaults) {
                if i >= offset {
                    if let Some(Some(d)) = defs.get(i - offset) {
                        value.push_str(default_sep);
                        value.push_str(&self.accept(self.child(g, *d)));
                    }
                }
            }
            values.push(value);
        }
        values.join(", ")
    }
}

fn import_string(md: &crate::graph::Module, names: &[(pyast::tree::Sym, Option<pyast::tree::Sym>)]) -> String {
    let parts: Vec<String> = names
        .iter()
        .map(|(n, asn)| match asn {
            Some(a) => format!("{} as {}", md.tree.s(*n), md.tree.s(*a)),
            None => md.tree.s(*n).to_string(),
        })
        .collect();
    parts.join(", ")
}

/// repr() of const values (visit_const)
pub fn const_repr(c: &ConstValue) -> String {
    match c {
        ConstValue::None => "None".to_string(),
        ConstValue::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        ConstValue::Ellipsis => "...".to_string(),
        ConstValue::NotImplemented => "NotImplemented".to_string(),
        ConstValue::Int(IntValue::Small(i)) => i.to_string(),
        ConstValue::Int(IntValue::Big(s)) => s.to_string(),
        ConstValue::Float(f) => pyast::pyrepr::repr_float(*f),
        ConstValue::Complex { real, imag } => complex_repr(*real, *imag),
        ConstValue::Str(s) => pyast::pyrepr::repr_str(s),
        ConstValue::StrSurrogate(p) => pyast::pyrepr::repr_str_points(p),
        ConstValue::Bytes(b) => bytes_repr(b),
    }
}

/// CPython complex_repr: real==+0.0 -> "<imag>j" (float repr WITHOUT the
/// forced ".0"); else "(<real>+<imag>j)"
fn complex_repr(real: f64, imag: f64) -> String {
    fn fl(v: f64) -> String {
        let s = pyast::pyrepr::repr_float(v);
        s.strip_suffix(".0").map(|x| x.to_string()).unwrap_or(s)
    }
    if real == 0.0 && real.is_sign_positive() {
        format!("{}j", fl(imag))
    } else {
        let im = fl(imag);
        if im.starts_with('-') {
            format!("({}{}j)", fl(real), im)
        } else {
            format!("({}+{}j)", fl(real), im)
        }
    }
}

/// python bytes repr
pub fn bytes_repr(b: &[u8]) -> String {
    let has_sq = b.contains(&b'\'');
    let has_dq = b.contains(&b'"');
    let quote = if has_sq && !has_dq { b'"' } else { b'\'' };
    let mut out = String::from("b");
    out.push(quote as char);
    for &c in b {
        match c {
            b'\\' => out.push_str("\\\\"),
            b'\t' => out.push_str("\\t"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c as char);
            }
            0x20..=0x7e => out.push(c as char),
            c => out.push_str(&format!("\\x{c:02x}")),
        }
    }
    out.push(quote as char);
    out
}
