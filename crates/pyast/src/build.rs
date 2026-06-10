//! Convert ruff's AST into the astroid-equivalent `Tree`.
//!
//! Position fidelity notes (vs astroid 4.0.4 / CPython 3.12):
//! - FunctionDef/ClassDef fromlineno/col = the `def`/`class` keyword (ruff
//!   ranges include decorators, so we scan tokens).
//! - Module: fromlineno 0, col 0, end_lineno/end_col None.
//! - Arguments/Comprehension: no own position (col None); fromlineno falls
//!   back to parent at finalize time, tolineno = last child.
//! - Docstrings: first Expr(Const str) of Module/Class/Function body is
//!   removed from body and stored as doc_node.
//! - ClassDef: `metaclass=` keyword extracted out of keywords.

use ruff_python_ast as ast;
use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::{Ranged, TextRange, TextSize};

use crate::source::SourceFile;
use crate::tree::*;

pub struct BuildOptions {
    pub modname: String,
    pub filepath: String,
    pub package: bool,
}

struct ScopeCtx {
    scope: NodeId,
    /// names declared `global` in the current function frame stack
    global_names: Vec<rustc_hash::FxHashSet<Sym>>,
}

pub struct Builder<'a> {
    src: &'a SourceFile,
    nodes: Vec<Node>,
    interner: Interner,
    locals: rustc_hash::FxHashMap<NodeId, indexmap::IndexMap<Sym, Vec<NodeId>>>,
    /// (module-level or not) ImportFrom nodes for delayed locals insertion
    delayed_import_from: Vec<NodeId>,
    /// token start offsets of `def` / `class` keywords, sorted
    def_class_tokens: Vec<(TextSize, bool)>, // (offset, is_def)
    /// token start offsets of `async` keywords, sorted
    async_tokens: Vec<(TextSize, bool)>,
    scope_stack: Vec<ScopeCtx>,
}

impl<'a> Builder<'a> {
    pub fn build(
        src: &'a SourceFile,
        parsed: &ast::ModModule,
        tokens: &[(TextSize, bool)],
        async_tokens: &[(TextSize, bool)],
        opts: &BuildOptions,
    ) -> Tree {
        let mut b = Builder {
            src,
            nodes: Vec::with_capacity(1024),
            interner: Interner::default(),
            locals: rustc_hash::FxHashMap::default(),
            delayed_import_from: Vec::new(),
            def_class_tokens: tokens.to_vec(),
            async_tokens: async_tokens.to_vec(),
            scope_stack: Vec::new(),
        };
        // Module node id 0
        let module_id = b.push_placeholder();
        b.locals.entry(module_id).or_default();
        b.scope_stack.push(ScopeCtx {
            scope: module_id,
            global_names: vec![],
        });

        let mut body: Vec<NodeId> = Vec::new();
        for stmt in &parsed.body {
            body.push(b.stmt(stmt, module_id));
        }
        let (doc_node, body) = b.extract_doc(body);

        let mut future_imports = Vec::new();
        for &id in &body {
            if let NodeKind::ImportFrom { modname, names, .. } = &b.nodes[id.idx()].kind {
                if b.interner.get(*modname) == "__future__" {
                    for (n, _) in names {
                        future_imports.push(*n);
                    }
                }
            }
        }

        b.nodes[module_id.idx()] = Node {
            kind: NodeKind::Module(Box::new(ModuleData {
                name: opts.modname.clone().into_boxed_str(),
                file: opts.filepath.clone().into_boxed_str(),
                package: opts.package,
                body,
                doc_node,
                future_imports,
            })),
            parent: module_id,
            fromlineno: 0,
            col_offset: 0,
            end_lineno: 0,
            end_col_offset: -1,
            tolineno: 0,
        };

        // delayed ImportFrom locals (astroid processes them in _post_build)
        for if_id in std::mem::take(&mut b.delayed_import_from) {
            b.delayed_import_from_locals(if_id);
        }

        let mut tree = Tree {
            nodes: b.nodes,
            interner: b.interner,
            locals: b.locals,
        };
        finalize_positions(&mut tree);
        tree
    }

    // ---------- infrastructure ----------

    fn push_placeholder(&mut self) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(Node {
            kind: NodeKind::Unknown,
            parent: NodeId::MODULE,
            fromlineno: 0,
            col_offset: -1,
            end_lineno: 0,
            end_col_offset: -1,
            tolineno: 0,
        });
        id
    }

    fn finish(&mut self, id: NodeId, kind: NodeKind, parent: NodeId, range: TextRange) -> NodeId {
        let (l1, c1) = self.src.line_col(range.start().to_u32());
        let (l2, c2) = self.src.line_col(range.end().to_u32());
        self.nodes[id.idx()] = Node {
            kind,
            parent,
            fromlineno: l1,
            col_offset: c1 as i32,
            end_lineno: l2,
            end_col_offset: c2 as i32,
            tolineno: 0,
        };
        id
    }

    fn finish_nopos(&mut self, id: NodeId, kind: NodeKind, parent: NodeId) -> NodeId {
        self.nodes[id.idx()] = Node {
            kind,
            parent,
            fromlineno: 0, // fixed up in finalize (parent fallback)
            col_offset: -1,
            end_lineno: 0,
            end_col_offset: -1,
            tolineno: 0,
        };
        id
    }

    fn sym(&mut self, s: &str) -> Sym {
        self.interner.intern(s)
    }

    fn cur_scope(&self) -> NodeId {
        self.scope_stack.last().unwrap().scope
    }

    fn set_local(&mut self, scope: NodeId, name: Sym, node: NodeId) {
        self.locals
            .entry(scope)
            .or_default()
            .entry(name)
            .or_default()
            .push(node);
    }

    /// astroid _save_assignment: if name is declared global in the current
    /// frame, assign to module locals instead.
    fn save_assignment(&mut self, name: Sym, node: NodeId) {
        let ctx = self.scope_stack.last().unwrap();
        let is_global = ctx.global_names.iter().any(|s| s.contains(&name));
        let scope = if is_global { NodeId::MODULE } else { ctx.scope };
        self.set_local(scope, name, node);
    }

    /// CPython gives `def`/`class` statements the keyword's position
    /// (decorators excluded; `async def` anchors at `async`). Ruff ranges
    /// include decorators, so find the first def/class (or preceding async)
    /// token inside the range.
    fn def_class_pos(&self, body_range: TextRange, is_async: bool) -> TextRange {
        let start = body_range.start();
        let mut found: Option<usize> = None;
        for (i, &(off, _)) in self.def_class_tokens.iter().enumerate() {
            if off >= start && off < body_range.end() {
                found = Some(i);
                break;
            }
        }
        match found {
            Some(i) => {
                let mut off = self.def_class_tokens[i].0;
                if is_async {
                    // anchor at the `async` token directly before
                    if i > 0 {
                        if let Some(&(aoff, _)) = self.async_tokens_before(off) {
                            off = aoff;
                        }
                    }
                }
                TextRange::new(off, body_range.end())
            }
            None => body_range,
        }
    }

    fn async_tokens_before(&self, def_off: TextSize) -> Option<&(TextSize, bool)> {
        // async token list: stored separately
        let idx = self
            .async_tokens
            .partition_point(|&(off, _)| off < def_off);
        if idx > 0 {
            Some(&self.async_tokens[idx - 1])
        } else {
            None
        }
    }

    fn extract_doc(&mut self, mut body: Vec<NodeId>) -> (Option<NodeId>, Vec<NodeId>) {
        if let Some(&first) = body.first() {
            if let NodeKind::Expr { value } = &self.nodes[first.idx()].kind {
                let v = *value;
                if matches!(
                    &self.nodes[v.idx()].kind,
                    NodeKind::Const(ConstValue::Str(_))
                ) {
                    body.remove(0);
                    return (Some(v), body);
                }
            }
        }
        (None, body)
    }

    fn delayed_import_from_locals(&mut self, if_id: NodeId) {
        // parent of the ImportFrom decides the scope (its frame); we recorded
        // them only to defer ordering. Wildcards handled later w/ manager.
        let parent = self.nodes[if_id.idx()].parent;
        let scope = self.frame_of(parent);
        if let NodeKind::ImportFrom { names, .. } = &self.nodes[if_id.idx()].kind {
            let names: Vec<(Sym, Option<Sym>)> = names.clone();
            for (name, asname) in names {
                let n = self.interner.get(name).to_string();
                if n == "*" {
                    continue; // wildcard: resolved with manager later
                }
                let local = asname.unwrap_or(name);
                self.set_local(scope, local, if_id);
            }
        }
    }

    fn frame_of(&self, mut id: NodeId) -> NodeId {
        loop {
            if self.locals.contains_key(&id)
                && !matches!(
                    self.nodes[id.idx()].kind,
                    NodeKind::ListComp(_)
                        | NodeKind::SetComp(_)
                        | NodeKind::DictComp(_)
                        | NodeKind::GeneratorExp(_)
                )
            {
                return id;
            }
            let p = self.nodes[id.idx()].parent;
            if p == id {
                return id;
            }
            id = p;
        }
    }

    // ---------- statements ----------

    fn stmts(&mut self, stmts: &[Stmt], parent: NodeId) -> Vec<NodeId> {
        stmts.iter().map(|s| self.stmt(s, parent)).collect()
    }

    fn stmt(&mut self, stmt: &Stmt, parent: NodeId) -> NodeId {
        let id = self.push_placeholder();
        match stmt {
            Stmt::FunctionDef(f) => self.function_def(id, f, parent),
            Stmt::ClassDef(c) => self.class_def(id, c, parent),
            Stmt::Return(r) => {
                let value = r.value.as_ref().map(|v| self.expr(v, id));
                self.finish(id, NodeKind::Return { value }, parent, r.range)
            }
            Stmt::Delete(d) => {
                let targets: Vec<NodeId> = d.targets.iter().map(|t| self.expr(t, id)).collect();
                self.finish(id, NodeKind::Delete { targets }, parent, d.range)
            }
            Stmt::Assign(a) => {
                let targets: Vec<NodeId> = a.targets.iter().map(|t| self.expr(t, id)).collect();
                let value = self.expr(&a.value, id);
                self.finish(id, NodeKind::Assign { targets, value }, parent, a.range)
            }
            Stmt::AugAssign(a) => {
                let target = self.expr(&a.target, id);
                let value = self.expr(&a.value, id);
                let op = format!("{}=", op_str(&a.op));
                self.finish(
                    id,
                    NodeKind::AugAssign {
                        target,
                        op: op.into_boxed_str(),
                        value,
                    },
                    parent,
                    a.range,
                )
            }
            Stmt::AnnAssign(a) => {
                let target = self.expr(&a.target, id);
                let annotation = self.expr(&a.annotation, id);
                let value = a.value.as_ref().map(|v| self.expr(v, id));
                self.finish(
                    id,
                    NodeKind::AnnAssign {
                        target,
                        annotation,
                        value,
                        simple: a.simple,
                    },
                    parent,
                    a.range,
                )
            }
            Stmt::TypeAlias(t) => {
                let name = self.expr(&t.name, id);
                let type_params = t
                    .type_params
                    .as_ref()
                    .map(|tp| self.type_params(tp, id))
                    .unwrap_or_default();
                let value = self.expr(&t.value, id);
                self.finish(
                    id,
                    NodeKind::TypeAlias {
                        name,
                        type_params,
                        value,
                    },
                    parent,
                    t.range,
                )
            }
            Stmt::For(f) => {
                let target = self.expr(&f.target, id);
                let iter = self.expr(&f.iter, id);
                let body = self.stmts(&f.body, id);
                let orelse = self.stmts(&f.orelse, id);
                let data = Box::new(ForData {
                    target,
                    iter,
                    body,
                    orelse,
                });
                let kind = if f.is_async {
                    NodeKind::AsyncFor(data)
                } else {
                    NodeKind::For(data)
                };
                self.finish(id, kind, parent, f.range)
            }
            Stmt::While(w) => {
                let test = self.expr(&w.test, id);
                let body = self.stmts(&w.body, id);
                let orelse = self.stmts(&w.orelse, id);
                self.finish(id, NodeKind::While { test, body, orelse }, parent, w.range)
            }
            Stmt::If(i) => self.if_stmt(id, i, parent),
            Stmt::With(w) => {
                let items: Vec<(NodeId, Option<NodeId>)> = w
                    .items
                    .iter()
                    .map(|item| {
                        let e = self.expr(&item.context_expr, id);
                        let v = item.optional_vars.as_ref().map(|v| self.expr(v, id));
                        (e, v)
                    })
                    .collect();
                let body = self.stmts(&w.body, id);
                let data = Box::new(WithData { items, body });
                let kind = if w.is_async {
                    NodeKind::AsyncWith(data)
                } else {
                    NodeKind::With(data)
                };
                self.finish(id, kind, parent, w.range)
            }
            Stmt::Match(m) => {
                let subject = self.expr(&m.subject, id);
                let cases: Vec<NodeId> = m.cases.iter().map(|c| self.match_case(c, id)).collect();
                self.finish(id, NodeKind::Match { subject, cases }, parent, m.range)
            }
            Stmt::Raise(r) => {
                let exc = r.exc.as_ref().map(|e| self.expr(e, id));
                let cause = r.cause.as_ref().map(|c| self.expr(c, id));
                self.finish(id, NodeKind::Raise { exc, cause }, parent, r.range)
            }
            Stmt::Try(t) => {
                let body = self.stmts(&t.body, id);
                let handlers: Vec<NodeId> = t
                    .handlers
                    .iter()
                    .map(|h| self.except_handler(h, id))
                    .collect();
                let orelse = self.stmts(&t.orelse, id);
                let finalbody = self.stmts(&t.finalbody, id);
                let data = Box::new(TryData {
                    body,
                    handlers,
                    orelse,
                    finalbody,
                });
                let kind = if t.is_star {
                    NodeKind::TryStar(data)
                } else {
                    NodeKind::Try(data)
                };
                self.finish(id, kind, parent, t.range)
            }
            Stmt::Assert(a) => {
                let test = self.expr(&a.test, id);
                let fail = a.msg.as_ref().map(|m| self.expr(m, id));
                self.finish(id, NodeKind::Assert { test, fail }, parent, a.range)
            }
            Stmt::Import(im) => {
                let names: Vec<(Sym, Option<Sym>)> = im
                    .names
                    .iter()
                    .map(|a| {
                        (
                            self.sym(a.name.as_str()),
                            a.asname.as_ref().map(|s| self.sym(s.as_str())),
                        )
                    })
                    .collect();
                let fin = self.finish(id, NodeKind::Import { names: names.clone() }, parent, im.range);
                // set_local: asname or first dotted part
                let scope = self.cur_scope();
                let _ = scope;
                for (name, asname) in &names {
                    let local = match asname {
                        Some(a) => *a,
                        None => {
                            let full = self.interner.get(*name).to_string();
                            let first = full.split('.').next().unwrap_or(&full).to_string();
                            self.sym(&first)
                        }
                    };
                    self.save_assignment(local, fin);
                }
                fin
            }
            Stmt::ImportFrom(im) => {
                let modname = self.sym(im.module.as_ref().map(|m| m.as_str()).unwrap_or(""));
                let names: Vec<(Sym, Option<Sym>)> = im
                    .names
                    .iter()
                    .map(|a| {
                        (
                            self.sym(a.name.as_str()),
                            a.asname.as_ref().map(|s| self.sym(s.as_str())),
                        )
                    })
                    .collect();
                let level = if im.level == 0 { None } else { Some(im.level) };
                let fin = self.finish(
                    id,
                    NodeKind::ImportFrom {
                        modname,
                        names,
                        level,
                    },
                    parent,
                    im.range,
                );
                self.delayed_import_from.push(fin);
                fin
            }
            Stmt::Global(g) => {
                let names: Vec<Sym> = g.names.iter().map(|n| self.sym(n.as_str())).collect();
                if let Some(set) = self
                    .scope_stack
                    .last_mut()
                    .unwrap()
                    .global_names
                    .last_mut()
                {
                    for n in &names {
                        set.insert(*n);
                    }
                }
                self.finish(id, NodeKind::Global { names }, parent, g.range)
            }
            Stmt::Nonlocal(n) => {
                let names: Vec<Sym> = n.names.iter().map(|x| self.sym(x.as_str())).collect();
                self.finish(id, NodeKind::Nonlocal { names }, parent, n.range)
            }
            Stmt::Expr(e) => {
                let value = self.expr(&e.value, id);
                self.finish(id, NodeKind::Expr { value }, parent, e.range)
            }
            Stmt::Pass(p) => self.finish(id, NodeKind::Pass, parent, p.range),
            Stmt::Break(b) => self.finish(id, NodeKind::Break, parent, b.range),
            Stmt::Continue(c) => self.finish(id, NodeKind::Continue, parent, c.range),
            Stmt::IpyEscapeCommand(c) => self.finish(id, NodeKind::Unknown, parent, c.range),
        }
    }

    fn if_stmt(&mut self, id: NodeId, i: &ast::StmtIf, parent: NodeId) -> NodeId {
        // ruff represents elif via elif_else_clauses; CPython/astroid nest Ifs.
        let test = self.expr(&i.test, id);
        let body = self.stmts(&i.body, id);
        let orelse = self.build_elif(&i.elif_else_clauses, id);
        self.finish(id, NodeKind::If { test, body, orelse }, parent, i.range)
    }

    fn build_elif(&mut self, clauses: &[ast::ElifElseClause], parent: NodeId) -> Vec<NodeId> {
        if clauses.is_empty() {
            return vec![];
        }
        let first = &clauses[0];
        match &first.test {
            Some(test_expr) => {
                // elif -> nested If spanning to the end of the remaining chain
                let id = self.push_placeholder();
                let test = self.expr(test_expr, id);
                let body = self.stmts(&first.body, id);
                let orelse = self.build_elif(&clauses[1..], id);
                // range: from elif keyword to end of last clause in chain
                let end = clauses.last().unwrap().range.end();
                let range = TextRange::new(first.range.start(), end);
                vec![self.finish(id, NodeKind::If { test, body, orelse }, parent, range)]
            }
            None => self.stmts(&first.body, parent),
        }
    }

    fn function_def(&mut self, id: NodeId, f: &ast::StmtFunctionDef, parent: NodeId) -> NodeId {
        let name = self.sym(f.name.as_str());
        // set_local in the parent scope (astroid: frame of parent)
        self.save_assignment(name, id);

        let decorators = self.decorators(&f.decorator_list, id);
        self.locals.entry(id).or_default();
        self.scope_stack.push(ScopeCtx {
            scope: id,
            global_names: vec![rustc_hash::FxHashSet::default()],
        });

        let args = self.arguments(&f.parameters, id);
        let returns = f.returns.as_ref().map(|r| self.expr(r, id));
        let type_params = f
            .type_params
            .as_ref()
            .map(|tp| self.type_params(tp, id))
            .unwrap_or_default();
        let body_ids = self.stmts(&f.body, id);
        let (doc_node, body) = self.extract_doc(body_ids);
        self.scope_stack.pop();

        let range = self.def_class_pos(f.range, f.is_async);
        let data = Box::new(FunctionData {
            name,
            decorators,
            args,
            returns,
            type_params,
            body,
            doc_node,
        });
        let kind = if f.is_async {
            NodeKind::AsyncFunctionDef(data)
        } else {
            NodeKind::FunctionDef(data)
        };
        self.finish(id, kind, parent, range)
    }

    fn class_def(&mut self, id: NodeId, c: &ast::StmtClassDef, parent: NodeId) -> NodeId {
        let name = self.sym(c.name.as_str());
        self.save_assignment(name, id);

        let decorators = self.decorators(&c.decorator_list, id);
        self.locals.entry(id).or_default();
        // implicit class locals, in astroid order
        for implicit in ["__module__", "__qualname__"] {
            let s = self.sym(implicit);
            self.locals.get_mut(&id).unwrap().entry(s).or_default();
        }
        self.scope_stack.push(ScopeCtx {
            scope: id,
            global_names: vec![],
        });

        let mut bases = Vec::new();
        let mut keywords = Vec::new();
        let mut metaclass = None;
        if let Some(args) = &c.arguments {
            for b in &args.args {
                bases.push(self.expr(b, id));
            }
            for kw in &args.keywords {
                let kid = self.keyword(kw, id);
                let is_meta = kw
                    .arg
                    .as_ref()
                    .map(|a| a.as_str() == "metaclass")
                    .unwrap_or(false);
                if is_meta {
                    metaclass = Some(match &self.nodes[kid.idx()].kind {
                        NodeKind::Keyword { value, .. } => *value,
                        _ => kid,
                    });
                } else {
                    keywords.push(kid);
                }
            }
        }
        let type_params = c
            .type_params
            .as_ref()
            .map(|tp| self.type_params(tp, id))
            .unwrap_or_default();
        let body_ids = self.stmts(&c.body, id);
        let (doc_node, body) = self.extract_doc(body_ids);

        // __annotations__ implicit local if any AnnAssign in body? Determined
        // empirically: astroid adds it when the class has annotated assigns.
        let has_ann = body.iter().any(|&s| matches!(self.nodes[s.idx()].kind, NodeKind::AnnAssign { .. }));
        if has_ann {
            let s = self.sym("__annotations__");
            let lm = self.locals.get_mut(&id).unwrap();
            // astroid inserts __annotations__ right after __qualname__
            let entry_exists = lm.contains_key(&s);
            if !entry_exists {
                let mut newmap = indexmap::IndexMap::new();
                let mut inserted = false;
                for (k, v) in lm.drain(..) {
                    newmap.insert(k, v);
                    if !inserted && self.interner.get(k) == "__qualname__" {
                        newmap.insert(s, vec![]);
                        inserted = true;
                    }
                }
                *lm = newmap;
            }
        }
        self.scope_stack.pop();

        let range = self.def_class_pos(c.range, false);
        self.finish(
            id,
            NodeKind::ClassDef(Box::new(ClassData {
                name,
                decorators,
                bases,
                keywords,
                metaclass,
                type_params,
                body,
                doc_node,
            })),
            parent,
            range,
        )
    }

    fn decorators(&mut self, list: &[ast::Decorator], parent: NodeId) -> Option<NodeId> {
        if list.is_empty() {
            return None;
        }
        let id = self.push_placeholder();
        let nodes: Vec<NodeId> = list.iter().map(|d| self.expr(&d.expression, id)).collect();
        let range = TextRange::new(
            list.first().unwrap().range.start(),
            list.last().unwrap().expression.range().end(),
        );
        Some(self.finish(id, NodeKind::Decorators { nodes }, parent, range))
    }

    fn arguments(&mut self, params: &ast::Parameters, parent: NodeId) -> NodeId {
        let id = self.push_placeholder();
        let mut posonlyargs = Vec::new();
        let mut posonlyargs_annotations = Vec::new();
        let mut args = Vec::new();
        let mut annotations = Vec::new();
        let mut defaults = Vec::new();
        let mut kwonlyargs = Vec::new();
        let mut kwonlyargs_annotations = Vec::new();
        let mut kw_defaults = Vec::new();

        for p in &params.posonlyargs {
            let an = self.param_name(&p.parameter, id);
            posonlyargs.push(an);
            posonlyargs_annotations
                .push(p.parameter.annotation.as_ref().map(|a| self.expr(a, id)));
            if let Some(d) = &p.default {
                defaults.push(self.expr(d, id));
            }
        }
        for p in &params.args {
            let an = self.param_name(&p.parameter, id);
            args.push(an);
            annotations.push(p.parameter.annotation.as_ref().map(|a| self.expr(a, id)));
            if let Some(d) = &p.default {
                defaults.push(self.expr(d, id));
            }
        }
        let (vararg, vararg_node, varargannotation) = match &params.vararg {
            Some(v) => {
                let name = self.sym(v.name.as_str());
                let ann = v.annotation.as_ref().map(|a| self.expr(a, id));
                (Some(name), None, ann)
            }
            None => (None, None, None),
        };
        for p in &params.kwonlyargs {
            let an = self.param_name(&p.parameter, id);
            kwonlyargs.push(an);
            kwonlyargs_annotations
                .push(p.parameter.annotation.as_ref().map(|a| self.expr(a, id)));
            kw_defaults.push(p.default.as_ref().map(|d| self.expr(d, id)));
        }
        let (kwarg, kwarg_node, kwargannotation) = match &params.kwarg {
            Some(v) => {
                let name = self.sym(v.name.as_str());
                let ann = v.annotation.as_ref().map(|a| self.expr(a, id));
                (Some(name), None, ann)
            }
            None => (None, None, None),
        };

        // locals order: posonly+args, kwonly, vararg, kwarg
        let scope = self.cur_scope();
        for &a in posonlyargs.iter().chain(args.iter()).chain(kwonlyargs.iter()) {
            if let NodeKind::AssignName { name } = self.nodes[a.idx()].kind {
                self.set_local(scope, name, a);
            }
        }
        if let Some(v) = vararg {
            self.set_local(scope, v, id);
        }
        if let Some(k) = kwarg {
            self.set_local(scope, k, id);
        }

        self.finish_nopos(
            id,
            NodeKind::Arguments(Box::new(ArgumentsData {
                posonlyargs,
                args,
                vararg,
                vararg_node,
                kwonlyargs,
                kwarg,
                kwarg_node,
                defaults,
                kw_defaults,
                annotations,
                posonlyargs_annotations,
                kwonlyargs_annotations,
                varargannotation,
                kwargannotation,
            })),
            parent,
        )
    }

    fn param_name(&mut self, p: &ast::Parameter, parent: NodeId) -> NodeId {
        let id = self.push_placeholder();
        let name = self.sym(p.name.as_str());
        self.finish(id, NodeKind::AssignName { name }, parent, p.name.range())
    }

    fn type_params(&mut self, tp: &ast::TypeParams, parent: NodeId) -> Vec<NodeId> {
        tp.type_params
            .iter()
            .map(|p| {
                let id = self.push_placeholder();
                match p {
                    ast::TypeParam::TypeVar(t) => {
                        let nid = self.push_placeholder();
                        let name = self.sym(t.name.as_str());
                        self.finish(nid, NodeKind::AssignName { name }, id, t.name.range());
                        self.set_local(id, name, nid);
                        let bound = t.bound.as_ref().map(|b| self.expr(b, id));
                        self.finish(id, NodeKind::TypeVar { name: nid, bound }, parent, t.range)
                    }
                    ast::TypeParam::ParamSpec(t) => {
                        let nid = self.push_placeholder();
                        let name = self.sym(t.name.as_str());
                        self.finish(nid, NodeKind::AssignName { name }, id, t.name.range());
                        self.finish(id, NodeKind::ParamSpec { name: nid }, parent, t.range)
                    }
                    ast::TypeParam::TypeVarTuple(t) => {
                        let nid = self.push_placeholder();
                        let name = self.sym(t.name.as_str());
                        self.finish(nid, NodeKind::AssignName { name }, id, t.name.range());
                        self.finish(id, NodeKind::TypeVarTuple { name: nid }, parent, t.range)
                    }
                }
            })
            .collect()
    }

    fn match_case(&mut self, c: &ast::MatchCase, parent: NodeId) -> NodeId {
        // astroid MatchCase has NO position (lineno=None)
        let id = self.push_placeholder();
        let pattern = self.pattern(&c.pattern, id);
        let guard = c.guard.as_ref().map(|g| self.expr(g, id));
        let body = self.stmts(&c.body, id);
        self.finish_nopos(
            id,
            NodeKind::MatchCase {
                pattern,
                guard,
                body,
            },
            parent,
        )
    }

    fn pattern(&mut self, p: &ast::Pattern, parent: NodeId) -> NodeId {
        let id = self.push_placeholder();
        match p {
            ast::Pattern::MatchValue(m) => {
                let value = self.expr(&m.value, id);
                self.finish(id, NodeKind::MatchValue { value }, parent, m.range)
            }
            ast::Pattern::MatchSingleton(m) => {
                let value = match m.value {
                    ast::Singleton::None => ConstValue::None,
                    ast::Singleton::True => ConstValue::Bool(true),
                    ast::Singleton::False => ConstValue::Bool(false),
                };
                self.finish(id, NodeKind::MatchSingleton { value }, parent, m.range)
            }
            ast::Pattern::MatchSequence(m) => {
                let patterns: Vec<NodeId> =
                    m.patterns.iter().map(|x| self.pattern(x, id)).collect();
                self.finish(id, NodeKind::MatchSequence { patterns }, parent, m.range)
            }
            ast::Pattern::MatchMapping(m) => {
                let keys: Vec<NodeId> = m.keys.iter().map(|k| self.expr(k, id)).collect();
                let patterns: Vec<NodeId> =
                    m.patterns.iter().map(|x| self.pattern(x, id)).collect();
                // astroid quirk: rest AssignName takes the MatchMapping's range
                let rest = m.rest.as_ref().map(|r| {
                    let rid = self.push_placeholder();
                    let name = self.sym(r.as_str());
                    let fin = self.finish(rid, NodeKind::AssignName { name }, id, m.range);
                    self.save_assignment(name, fin);
                    fin
                });
                self.finish(
                    id,
                    NodeKind::MatchMapping {
                        keys,
                        patterns,
                        rest,
                    },
                    parent,
                    m.range,
                )
            }
            ast::Pattern::MatchClass(m) => {
                let cls = self.expr(&m.cls, id);
                let mut patterns = Vec::new();
                let mut kwd_attrs = Vec::new();
                let mut kwd_patterns = Vec::new();
                for pat in &m.arguments.patterns {
                    patterns.push(self.pattern(pat, id));
                }
                for kw in &m.arguments.keywords {
                    kwd_attrs.push(self.sym(kw.attr.as_str()));
                    kwd_patterns.push(self.pattern(&kw.pattern, id));
                }
                self.finish(
                    id,
                    NodeKind::MatchClass {
                        cls,
                        patterns,
                        kwd_attrs,
                        kwd_patterns,
                    },
                    parent,
                    m.range,
                )
            }
            ast::Pattern::MatchStar(m) => {
                // astroid quirk: name AssignName takes the MatchStar's range
                let name = m.name.as_ref().map(|n| {
                    let nid = self.push_placeholder();
                    let s = self.sym(n.as_str());
                    let fin = self.finish(nid, NodeKind::AssignName { name: s }, id, m.range);
                    self.save_assignment(s, fin);
                    fin
                });
                self.finish(id, NodeKind::MatchStar { name }, parent, m.range)
            }
            ast::Pattern::MatchAs(m) => {
                let pattern = m.pattern.as_ref().map(|x| self.pattern(x, id));
                // astroid quirk: name AssignName takes the MatchAs's range
                let name = m.name.as_ref().map(|n| {
                    let nid = self.push_placeholder();
                    let s = self.sym(n.as_str());
                    let fin = self.finish(nid, NodeKind::AssignName { name: s }, id, m.range);
                    self.save_assignment(s, fin);
                    fin
                });
                self.finish(id, NodeKind::MatchAs { pattern, name }, parent, m.range)
            }
            ast::Pattern::MatchOr(m) => {
                let patterns: Vec<NodeId> =
                    m.patterns.iter().map(|x| self.pattern(x, id)).collect();
                self.finish(id, NodeKind::MatchOr { patterns }, parent, m.range)
            }
        }
    }

    fn except_handler(&mut self, h: &ast::ExceptHandler, parent: NodeId) -> NodeId {
        let ast::ExceptHandler::ExceptHandler(h) = h;
        let id = self.push_placeholder();
        let type_ = h.type_.as_ref().map(|t| self.expr(t, id));
        // astroid quirk: the `as name` AssignName takes the WHOLE handler's range
        let name = h.name.as_ref().map(|n| {
            let nid = self.push_placeholder();
            let s = self.sym(n.as_str());
            let fin = self.finish(nid, NodeKind::AssignName { name: s }, id, h.range);
            self.save_assignment(s, fin);
            fin
        });
        let body = self.stmts(&h.body, id);
        self.finish(id, NodeKind::ExceptHandler { type_, name, body }, parent, h.range)
    }

    fn keyword(&mut self, kw: &ast::Keyword, parent: NodeId) -> NodeId {
        let id = self.push_placeholder();
        let arg = kw.arg.as_ref().map(|a| self.sym(a.as_str()));
        let value = self.expr(&kw.value, id);
        self.finish(id, NodeKind::Keyword { arg, value }, parent, kw.range)
    }

    fn comprehension_scope(
        &mut self,
        id: NodeId,
        generators: &[ast::Comprehension],
    ) -> Vec<NodeId> {
        self.locals.entry(id).or_default();
        self.scope_stack.push(ScopeCtx {
            scope: id,
            global_names: vec![],
        });
        let out: Vec<NodeId> = generators
            .iter()
            .map(|g| {
                let gid = self.push_placeholder();
                let target = self.expr(&g.target, gid);
                let iter = self.expr(&g.iter, gid);
                let ifs: Vec<NodeId> = g.ifs.iter().map(|i| self.expr(i, gid)).collect();
                self.finish_nopos(
                    gid,
                    NodeKind::Comprehension {
                        target,
                        iter,
                        ifs,
                        is_async: g.is_async,
                    },
                    id,
                )
            })
            .collect();
        out
    }

    // ---------- expressions ----------

    fn exprs(&mut self, exprs: &[Expr], parent: NodeId) -> Vec<NodeId> {
        exprs.iter().map(|e| self.expr(e, parent)).collect()
    }

    fn expr(&mut self, expr: &Expr, parent: NodeId) -> NodeId {
        let id = self.push_placeholder();
        match expr {
            Expr::BoolOp(b) => {
                let values = self.exprs(&b.values, id);
                let op = match b.op {
                    ast::BoolOp::And => "and",
                    ast::BoolOp::Or => "or",
                };
                self.finish(
                    id,
                    NodeKind::BoolOp {
                        op: op.into(),
                        values,
                    },
                    parent,
                    b.range,
                )
            }
            Expr::Named(n) => {
                let target = self.expr(&n.target, id);
                let value = self.expr(&n.value, id);
                self.finish(id, NodeKind::NamedExpr { target, value }, parent, n.range)
            }
            Expr::BinOp(b) => {
                let left = self.expr(&b.left, id);
                let right = self.expr(&b.right, id);
                self.finish(
                    id,
                    NodeKind::BinOp {
                        left,
                        op: op_str(&b.op).into(),
                        right,
                    },
                    parent,
                    b.range,
                )
            }
            Expr::UnaryOp(u) => {
                let operand = self.expr(&u.operand, id);
                let op = match u.op {
                    ast::UnaryOp::Invert => "~",
                    ast::UnaryOp::Not => "not",
                    ast::UnaryOp::UAdd => "+",
                    ast::UnaryOp::USub => "-",
                };
                self.finish(
                    id,
                    NodeKind::UnaryOp {
                        op: op.into(),
                        operand,
                    },
                    parent,
                    u.range,
                )
            }
            Expr::Lambda(l) => {
                self.locals.entry(id).or_default();
                self.scope_stack.push(ScopeCtx {
                    scope: id,
                    global_names: vec![rustc_hash::FxHashSet::default()],
                });
                let args = match &l.parameters {
                    Some(p) => self.arguments(p, id),
                    None => {
                        let aid = self.push_placeholder();
                        self.finish_nopos(
                            aid,
                            NodeKind::Arguments(Box::new(ArgumentsData {
                                posonlyargs: vec![],
                                args: vec![],
                                vararg: None,
                                vararg_node: None,
                                kwonlyargs: vec![],
                                kwarg: None,
                                kwarg_node: None,
                                defaults: vec![],
                                kw_defaults: vec![],
                                annotations: vec![],
                                posonlyargs_annotations: vec![],
                                kwonlyargs_annotations: vec![],
                                varargannotation: None,
                                kwargannotation: None,
                            })),
                            id,
                        )
                    }
                };
                let body = self.expr(&l.body, id);
                self.scope_stack.pop();
                self.finish(id, NodeKind::Lambda(Box::new(LambdaData { args, body })), parent, l.range)
            }
            Expr::If(i) => {
                let test = self.expr(&i.test, id);
                let body = self.expr(&i.body, id);
                let orelse = self.expr(&i.orelse, id);
                self.finish(id, NodeKind::IfExp { test, body, orelse }, parent, i.range)
            }
            Expr::Dict(d) => {
                let items: Vec<(NodeId, NodeId)> = d
                    .items
                    .iter()
                    .map(|item| {
                        let k = match &item.key {
                            Some(k) => self.expr(k, id),
                            None => {
                                // `**x`: astroid DictUnpack placeholder with the
                                // value's position
                                let uid = self.push_placeholder();
                                self.finish(uid, NodeKind::DictUnpack, id, item.value.range())
                            }
                        };
                        let v = self.expr(&item.value, id);
                        (k, v)
                    })
                    .collect();
                self.finish(id, NodeKind::Dict { items }, parent, d.range)
            }
            Expr::Set(s) => {
                let elts = self.exprs(&s.elts, id);
                self.finish(id, NodeKind::Set { elts }, parent, s.range)
            }
            Expr::ListComp(c) => {
                let generators = self.comprehension_scope(id, &c.generators);
                let elt = self.expr(&c.elt, id);
                self.scope_stack.pop();
                self.finish(
                    id,
                    NodeKind::ListComp(Box::new(CompData { elt, generators })),
                    parent,
                    c.range,
                )
            }
            Expr::SetComp(c) => {
                let generators = self.comprehension_scope(id, &c.generators);
                let elt = self.expr(&c.elt, id);
                self.scope_stack.pop();
                self.finish(
                    id,
                    NodeKind::SetComp(Box::new(CompData { elt, generators })),
                    parent,
                    c.range,
                )
            }
            Expr::DictComp(c) => {
                let generators = self.comprehension_scope(id, &c.generators);
                let key = match &c.key {
                    Some(k) => self.expr(k, id),
                    None => {
                        let uid = self.push_placeholder();
                        self.finish(uid, NodeKind::Unknown, id, c.range)
                    }
                };
                let value = self.expr(&c.value, id);
                self.scope_stack.pop();
                self.finish(
                    id,
                    NodeKind::DictComp(Box::new(DictCompData {
                        key,
                        value,
                        generators,
                    })),
                    parent,
                    c.range,
                )
            }
            Expr::Generator(c) => {
                let generators = self.comprehension_scope(id, &c.generators);
                let elt = self.expr(&c.elt, id);
                self.scope_stack.pop();
                self.finish(
                    id,
                    NodeKind::GeneratorExp(Box::new(CompData { elt, generators })),
                    parent,
                    c.range,
                )
            }
            Expr::Await(a) => {
                let value = self.expr(&a.value, id);
                self.finish(id, NodeKind::Await { value }, parent, a.range)
            }
            Expr::Yield(y) => {
                let value = y.value.as_ref().map(|v| self.expr(v, id));
                self.finish(id, NodeKind::Yield { value }, parent, y.range)
            }
            Expr::YieldFrom(y) => {
                let value = self.expr(&y.value, id);
                self.finish(id, NodeKind::YieldFrom { value }, parent, y.range)
            }
            Expr::Compare(c) => {
                let left = self.expr(&c.left, id);
                let ops: Vec<(Box<str>, NodeId)> = c
                    .ops
                    .iter()
                    .zip(c.comparators.iter())
                    .map(|(op, cmp)| {
                        let o: Box<str> = cmp_str(op).into();
                        (o, self.expr(cmp, id))
                    })
                    .collect();
                self.finish(id, NodeKind::Compare { left, ops }, parent, c.range)
            }
            Expr::Call(c) => {
                let func = self.expr(&c.func, id);
                let args = self.exprs(&c.arguments.args, id);
                let keywords: Vec<NodeId> = c
                    .arguments
                    .keywords
                    .iter()
                    .map(|k| self.keyword(k, id))
                    .collect();
                self.finish(
                    id,
                    NodeKind::Call {
                        func,
                        args,
                        keywords,
                    },
                    parent,
                    c.range,
                )
            }
            Expr::FString(f) => self.fstring(id, f, parent),
            Expr::StringLiteral(s) => {
                let v: String = s.value.to_str().to_string();
                self.finish(
                    id,
                    NodeKind::Const(ConstValue::Str(v.into_boxed_str())),
                    parent,
                    s.range,
                )
            }
            Expr::BytesLiteral(b) => {
                let mut v: Vec<u8> = Vec::new();
                for part in &b.value {
                    v.extend_from_slice(part.as_slice());
                }
                self.finish(
                    id,
                    NodeKind::Const(ConstValue::Bytes(v.into_boxed_slice())),
                    parent,
                    b.range,
                )
            }
            Expr::NumberLiteral(n) => {
                let cv = match &n.value {
                    ast::Number::Int(i) => match i.as_i64() {
                        Some(v) => ConstValue::Int(IntValue::Small(v)),
                        None => ConstValue::Int(IntValue::Big(i.to_string().into_boxed_str())),
                    },
                    ast::Number::Float(f) => ConstValue::Float(*f),
                    ast::Number::Complex { real, imag } => ConstValue::Complex {
                        real: *real,
                        imag: *imag,
                    },
                };
                self.finish(id, NodeKind::Const(cv), parent, n.range)
            }
            Expr::BooleanLiteral(b) => {
                self.finish(id, NodeKind::Const(ConstValue::Bool(b.value)), parent, b.range)
            }
            Expr::NoneLiteral(n) => {
                self.finish(id, NodeKind::Const(ConstValue::None), parent, n.range)
            }
            Expr::EllipsisLiteral(e) => {
                self.finish(id, NodeKind::Const(ConstValue::Ellipsis), parent, e.range)
            }
            Expr::Attribute(a) => {
                let e = self.expr(&a.value, id);
                let attrname = self.sym(a.attr.as_str());
                let kind = match a.ctx {
                    ast::ExprContext::Store => NodeKind::AssignAttr { expr: e, attrname },
                    ast::ExprContext::Del => NodeKind::DelAttr { expr: e, attrname },
                    _ => NodeKind::Attribute {
                        expr: e,
                        attrname,
                        ctx: Ctx::Load,
                    },
                };
                self.finish(id, kind, parent, a.range)
            }
            Expr::Subscript(s) => {
                let value = self.expr(&s.value, id);
                let slice = self.expr(&s.slice, id);
                let ctx = ctx_of(&s.ctx);
                self.finish(id, NodeKind::Subscript { value, slice, ctx }, parent, s.range)
            }
            Expr::Starred(s) => {
                let value = self.expr(&s.value, id);
                let ctx = ctx_of(&s.ctx);
                self.finish(id, NodeKind::Starred { value, ctx }, parent, s.range)
            }
            Expr::Name(n) => {
                let name = self.sym(n.id.as_str());
                let kind = match n.ctx {
                    ast::ExprContext::Store => NodeKind::AssignName { name },
                    ast::ExprContext::Del => NodeKind::DelName { name },
                    _ => NodeKind::Name { name },
                };
                let fin = self.finish(id, kind, parent, n.range);
                match n.ctx {
                    ast::ExprContext::Store => self.save_assignment(name, fin),
                    ast::ExprContext::Del => self.save_assignment(name, fin),
                    _ => {}
                }
                fin
            }
            Expr::List(l) => {
                let elts = self.exprs(&l.elts, id);
                self.finish(
                    id,
                    NodeKind::List {
                        elts,
                        ctx: ctx_of(&l.ctx),
                    },
                    parent,
                    l.range,
                )
            }
            Expr::Tuple(t) => {
                let elts = self.exprs(&t.elts, id);
                self.finish(
                    id,
                    NodeKind::Tuple {
                        elts,
                        ctx: ctx_of(&t.ctx),
                    },
                    parent,
                    t.range,
                )
            }
            Expr::Slice(s) => {
                let lower = s.lower.as_ref().map(|e| self.expr(e, id));
                let upper = s.upper.as_ref().map(|e| self.expr(e, id));
                let step = s.step.as_ref().map(|e| self.expr(e, id));
                self.finish(id, NodeKind::Slice { lower, upper, step }, parent, s.range)
            }
            Expr::IpyEscapeCommand(c) => self.finish(id, NodeKind::Unknown, parent, c.range),
            Expr::TString(t) => self.finish(id, NodeKind::Unknown, parent, t.range()),
        }
    }

    fn fstring(&mut self, id: NodeId, f: &ast::ExprFString, parent: NodeId) -> NodeId {
        // CPython: JoinedStr with Constant + FormattedValue children.
        // Adjacent literal string parts and literal fstring elements merge.
        let mut values: Vec<NodeId> = Vec::new();
        let mut pending: Option<(String, TextRange)> = None;
        let mut flush = |b: &mut Builder, values: &mut Vec<NodeId>, pending: &mut Option<(String, TextRange)>| {
            if let Some((s, range)) = pending.take() {
                let cid = b.push_placeholder();
                b.finish(
                    cid,
                    NodeKind::Const(ConstValue::Str(s.into_boxed_str())),
                    id,
                    range,
                );
                values.push(cid);
            }
        };
        for part in f.value.iter() {
            match part {
                ast::FStringPart::Literal(lit) => {
                    match &mut pending {
                        Some((s, range)) => {
                            s.push_str(lit.as_str());
                            *range = TextRange::new(range.start(), lit.range.end());
                        }
                        None => pending = Some((lit.as_str().to_string(), lit.range)),
                    }
                }
                ast::FStringPart::FString(fs) => {
                    for elem in fs.elements.iter() {
                        match elem {
                            ast::InterpolatedStringElement::Literal(lit) => match &mut pending {
                                Some((s, range)) => {
                                    s.push_str(&lit.value);
                                    *range = TextRange::new(range.start(), lit.range.end());
                                }
                                None => {
                                    pending = Some((lit.value.to_string(), lit.range))
                                }
                            },
                            ast::InterpolatedStringElement::Interpolation(fv) => {
                                flush(self, &mut values, &mut pending);
                                let fid = self.push_placeholder();
                                let value = self.expr(&fv.expression, fid);
                                let conversion = fv.conversion as i32;
                                let format_spec = fv.format_spec.as_ref().map(|spec| {
                                    let sid = self.push_placeholder();
                                    let mut spec_values: Vec<NodeId> = Vec::new();
                                    let mut spending: Option<(String, TextRange)> = None;
                                    for se in spec.elements.iter() {
                                        match se {
                                            ast::InterpolatedStringElement::Literal(lit) => {
                                                match &mut spending {
                                                    Some((s, range)) => {
                                                        s.push_str(&lit.value);
                                                        *range = TextRange::new(
                                                            range.start(),
                                                            lit.range.end(),
                                                        );
                                                    }
                                                    None => {
                                                        spending = Some((
                                                            lit.value.to_string(),
                                                            lit.range,
                                                        ))
                                                    }
                                                }
                                            }
                                            ast::InterpolatedStringElement::Interpolation(
                                                sfv,
                                            ) => {
                                                if let Some((s, range)) = spending.take() {
                                                    let cid = self.push_placeholder();
                                                    self.finish(
                                                        cid,
                                                        NodeKind::Const(ConstValue::Str(
                                                            s.into_boxed_str(),
                                                        )),
                                                        sid,
                                                        range,
                                                    );
                                                    spec_values.push(cid);
                                                }
                                                let sfid = self.push_placeholder();
                                                let sval = self.expr(&sfv.expression, sfid);
                                                self.finish(
                                                    sfid,
                                                    NodeKind::FormattedValue {
                                                        value: sval,
                                                        conversion: sfv.conversion as i32,
                                                        format_spec: None,
                                                    },
                                                    sid,
                                                    sfv.range,
                                                );
                                                spec_values.push(sfid);
                                            }
                                        }
                                    }
                                    if let Some((s, range)) = spending.take() {
                                        let cid = self.push_placeholder();
                                        self.finish(
                                            cid,
                                            NodeKind::Const(ConstValue::Str(s.into_boxed_str())),
                                            sid,
                                            range,
                                        );
                                        spec_values.push(cid);
                                    }
                                    // CPython positions the format_spec
                                    // JoinedStr starting AT the colon
                                    let spec_range = TextRange::new(
                                        spec.range.start() - TextSize::from(1),
                                        spec.range.end(),
                                    );
                                    self.finish(
                                        sid,
                                        NodeKind::JoinedStr {
                                            values: spec_values,
                                        },
                                        fid,
                                        spec_range,
                                    )
                                });
                                self.finish(
                                    fid,
                                    NodeKind::FormattedValue {
                                        value,
                                        conversion,
                                        format_spec,
                                    },
                                    id,
                                    fv.range,
                                );
                                values.push(fid);
                            }
                        }
                    }
                }
            }
        }
        flush(self, &mut values, &mut pending);
        self.finish(id, NodeKind::JoinedStr { values }, parent, f.range)
    }
}

fn ctx_of(ctx: &ast::ExprContext) -> Ctx {
    match ctx {
        ast::ExprContext::Store => Ctx::Store,
        ast::ExprContext::Del => Ctx::Del,
        _ => Ctx::Load,
    }
}

fn op_str(op: &ast::Operator) -> &'static str {
    match op {
        ast::Operator::Add => "+",
        ast::Operator::Sub => "-",
        ast::Operator::Mult => "*",
        ast::Operator::MatMult => "@",
        ast::Operator::Div => "/",
        ast::Operator::Mod => "%",
        ast::Operator::Pow => "**",
        ast::Operator::LShift => "<<",
        ast::Operator::RShift => ">>",
        ast::Operator::BitOr => "|",
        ast::Operator::BitXor => "^",
        ast::Operator::BitAnd => "&",
        ast::Operator::FloorDiv => "//",
    }
}

fn cmp_str(op: &ast::CmpOp) -> &'static str {
    match op {
        ast::CmpOp::Eq => "==",
        ast::CmpOp::NotEq => "!=",
        ast::CmpOp::Lt => "<",
        ast::CmpOp::LtE => "<=",
        ast::CmpOp::Gt => ">",
        ast::CmpOp::GtE => ">=",
        ast::CmpOp::Is => "is",
        ast::CmpOp::IsNot => "is not",
        ast::CmpOp::In => "in",
        ast::CmpOp::NotIn => "not in",
    }
}

/// Post-pass mirroring astroid's lazy position properties:
/// - fromlineno for positionless nodes (`lineno is None`): descend the
///   first-child chain until a node with a line is found; if no children,
///   walk up parents (astroid NodeNG._fixed_source_line).
/// - tolineno: end_lineno, else last child's tolineno, else fromlineno.
fn finalize_positions(tree: &mut Tree) {
    fn fixed_source_line(tree: &Tree, id: NodeId) -> u32 {
        // positioned node: col_offset >= 0 means it has a real position
        let mut cur = id;
        loop {
            let n = &tree.nodes[cur.idx()];
            if n.col_offset >= 0 {
                return n.fromlineno;
            }
            let children = tree.children(cur);
            match children.first() {
                Some(&c) => cur = c,
                None => break,
            }
        }
        // StopIteration: walk up parents reading lineno
        let mut p = tree.nodes[id.idx()].parent;
        loop {
            let n = &tree.nodes[p.idx()];
            if n.col_offset >= 0 || p == NodeId::MODULE {
                return n.fromlineno;
            }
            let next = n.parent;
            if next == p {
                return 0;
            }
            p = next;
        }
    }

    fn rec(tree: &mut Tree, id: NodeId) {
        if tree.nodes[id.idx()].col_offset < 0 && id != NodeId::MODULE {
            tree.nodes[id.idx()].fromlineno = fixed_source_line(tree, id);
        }
        let children = tree.children(id);
        for c in &children {
            rec(tree, *c);
        }
        let n = &tree.nodes[id.idx()];
        let tol = if n.end_lineno != 0 {
            n.end_lineno
        } else if let Some(last) = children.last() {
            tree.nodes[last.idx()].tolineno
        } else {
            n.fromlineno
        };
        tree.nodes[id.idx()].tolineno = tol;
    }
    rec(tree, NodeId::MODULE);
}
