//! Name lookup: LookupMixIn.lookup -> scope_lookup -> _filter_stmts.
//! Ports: astroid/nodes/_base_nodes.py:259-290, scoped_nodes mixin.py:78-98,
//! per-scope overrides (scoped_nodes.py), filter_statements.py:50-240.

use std::rc::Rc;

use pyast::tree::NodeKind;
use pyast::NodeId;

use crate::graph::Engine;
use crate::value::{GNode, GSym, NV};

pub type LookupResult = (GNode, Vec<NV>);

impl Engine {
    /// LookupMixIn.lookup — `@lru_cache` with the DEFAULT maxsize=128
    /// (_base_nodes.py:262). Exact functools semantics: hits refresh
    /// recency; an insert at capacity evicts the least-recently-used
    /// entry. Evictions matter: re-misses recompute against LIVE locals
    /// (cross-module delayed_assattr mutations) and re-mint fresh
    /// module-model Consts (__name__ hop bumps fire again).
    pub fn lookup(&self, node: GNode, name: GSym) -> Rc<LookupResult> {
        let tick = self.lookup_tick.get() + 1;
        self.lookup_tick.set(tick);
        if let Some(entry) = self.lookup_cache.borrow_mut().get_mut(&(node, name)) {
            self.lookup_evict.borrow_mut().touch(entry.1, tick, (node, name));
            entry.1 = tick;
            return Rc::clone(&entry.0);
        }
        let scope = self.scope(node);
        let res = Rc::new(self.scope_lookup(scope, node, name, 0));
        let mut cache = self.lookup_cache.borrow_mut();
        if cache.len() >= 128 {
            if let Some(oldest) = self.lookup_evict.borrow_mut().pop_lru() {
                cache.remove(&oldest);
            }
        }
        let mut evict = self.lookup_evict.borrow_mut();
        if let Some(old) = cache.insert((node, name), (Rc::clone(&res), tick)) {
            evict.forget(old.1);
        }
        evict.insert(tick, (node, name));
        res
    }

    /// protocols.py:540-546 (excepthandler_assigned_stmts, TryStar branch):
    /// `next(unpack_infer(extract_node("from builtins import ExceptionGroup
    /// \nExceptionGroup")))` — the throwaway Name's _infer calls
    /// LookupMixIn.lookup, INSERTING a never-hit-again (node, name) key
    /// into the global 128-LRU and evicting the LRU entry. The throwaway
    /// module build applies no transforms (no Name/ImportFrom/Module
    /// predicates match -> no cache wipe) and every infer pull inside
    /// unpack_infer is abandoned mid-stream (no global cache writes), so
    /// this eviction is the ONLY persistent global side effect to replay.
    pub fn lookup_burn_throwaway(&self, name: GSym) {
        let node = self.alloc_synth_node(NodeKind::Unknown);
        let tick = self.lookup_tick.get() + 1;
        self.lookup_tick.set(tick);
        let res: Rc<LookupResult> = Rc::new((node, Vec::new()));
        let mut cache = self.lookup_cache.borrow_mut();
        if cache.len() >= 128 {
            if let Some(oldest) = self.lookup_evict.borrow_mut().pop_lru() {
                cache.remove(&oldest);
            }
        }
        let mut evict = self.lookup_evict.borrow_mut();
        if let Some(old) = cache.insert((node, name), (res, tick)) {
            evict.forget(old.1);
        }
        evict.insert(tick, (node, name));
    }

    /// dispatch over the scope node type (notes/07 §7.3)
    pub fn scope_lookup(&self, scope: GNode, node: GNode, name: GSym, offset: i32) -> LookupResult {
        let md = self.md(scope.m);
        match &md.tree.nodes[scope.n.idx()].kind {
            NodeKind::Module(_) => self.module_scope_lookup(scope, node, name, offset),
            NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) => {
                self.function_scope_lookup(scope, node, name, offset)
            }
            NodeKind::Lambda(_) => self.lambda_scope_lookup(scope, node, name, offset),
            NodeKind::ClassDef(_) => self.class_scope_lookup(scope, node, name, offset),
            _ => self.base_scope_lookup(scope, node, name, offset),
        }
    }

    /// Module.scope_lookup (scoped_nodes.py:312-333)
    fn module_scope_lookup(&self, scope: GNode, node: GNode, name: GSym, offset: i32) -> LookupResult {
        let name_str = self.sname(name);
        if matches!(
            name_str.as_str(),
            "__name__" | "__doc__" | "__file__" | "__path__" | "__package__"
        ) {
            let shadowed = {
                let md = self.md(scope.m);
                let locals = md.locals.borrow();
                locals
                    .get(&scope.n)
                    .map(|l| l.contains_key(&name))
                    .unwrap_or(false)
            };
            if !shadowed {
                return match self.module_getattr(scope.m, name, false) {
                    Ok(vals) => (scope, vals),
                    Err(_) => (scope, Vec::new()),
                };
            }
        }
        self.base_scope_lookup(scope, node, name, offset)
    }

    /// FunctionDef.scope_lookup (scoped_nodes.py:1658-1682)
    fn function_scope_lookup(&self, scope: GNode, node: GNode, name: GSym, _offset: i32) -> LookupResult {
        let md = self.md(scope.m);
        if self.sname(name) == "__class__" {
            if let Some(p) = self.parent(scope) {
                let frame = self.frame(p);
                if self.kind_is(frame, |k| matches!(k, NodeKind::ClassDef(_))) {
                    return (scope, vec![NV::N(frame)]);
                }
            }
        }
        let in_defaults = {
            match &md.tree.nodes[scope.n.idx()].kind {
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                    match &md.tree.nodes[d.args.idx()].kind {
                        NodeKind::Arguments(a) => {
                            a.defaults.contains(&node.n)
                                || a.kw_defaults.iter().flatten().any(|&x| x == node.n)
                        }
                        _ => false,
                    }
                }
                _ => false,
            }
        };
        if in_defaults && node.m == scope.m {
            if let Some(p) = self.parent(scope) {
                let frame = self.frame(p);
                return self.base_scope_lookup(frame, node, name, -1);
            }
        }
        self.base_scope_lookup(scope, node, name, 0)
    }

    /// Lambda.scope_lookup (scoped_nodes.py:995-1023)
    fn lambda_scope_lookup(&self, scope: GNode, node: GNode, name: GSym, _offset: i32) -> LookupResult {
        let md = self.md(scope.m);
        let in_defaults = match &md.tree.nodes[scope.n.idx()].kind {
            NodeKind::Lambda(d) => match &md.tree.nodes[d.args.idx()].kind {
                NodeKind::Arguments(a) => {
                    a.defaults.contains(&node.n)
                        || a.kw_defaults.iter().flatten().any(|&x| x == node.n)
                }
                _ => false,
            },
            _ => false,
        };
        if in_defaults && node.m == scope.m {
            if let Some(p) = self.parent(scope) {
                let frame = self.frame(p);
                return self.base_scope_lookup(frame, node, name, -1);
            }
        }
        self.base_scope_lookup(scope, node, name, 0)
    }

    /// ClassDef.scope_lookup (scoped_nodes.py:2104-2155)
    fn class_scope_lookup(&self, scope: GNode, node: GNode, name: GSym, _offset: i32) -> LookupResult {
        let md = self.md(scope.m);
        let (bases, has_type_params): (Vec<NodeId>, bool) =
            match &md.tree.nodes[scope.n.idx()].kind {
                NodeKind::ClassDef(d) => (d.bases.clone(), !d.type_params.is_empty()),
                _ => (Vec::new(), false),
            };
        let lookup_upper_frame = {
            let parent_is_decorators = self
                .parent(node)
                .map(|p| self.kind_is(p, |k| matches!(k, NodeKind::Decorators { .. })))
                .unwrap_or(false);
            parent_is_decorators && {
                let bsym = name;
                let bm = self.md(self.builtins_mod.get());
                let locals = bm.locals.borrow();
                locals
                    .get(&NodeId::MODULE)
                    .map(|l| l.contains_key(&bsym))
                    .unwrap_or(false)
            }
        };
        let in_bases = node.m == scope.m
            && bases.iter().any(|&b| {
                b == node.n
                    || (self.parent_of(GNode { m: scope.m, n: b }, node) && !has_type_params)
            });
        if in_bases || lookup_upper_frame {
            if let Some(p) = self.parent(scope) {
                let frame = self.frame(p);
                return self.base_scope_lookup(frame, node, name, -1);
            }
        }
        self.base_scope_lookup(scope, node, name, 0)
    }

    /// mixin.py:78-98 _scope_lookup
    pub fn base_scope_lookup(&self, scope: GNode, node: GNode, name: GSym, offset: i32) -> LookupResult {
        let stmts = {
            // ClassDef.implicit_locals() (scoped_nodes.py:1911-1933): the
            // implicit __module__/__qualname__ Const + __annotations__
            // Unknown sit FIRST in every class's locals — Name lookups
            // inside the class body resolve them
            let implicit: Option<GNode> = if self
                .kind_is(scope, |k| matches!(k, NodeKind::ClassDef(_)))
            {
                match self.sname(name).as_str() {
                    "__module__" => Some(self.implicit_class_local(scope, 0)),
                    "__qualname__" => Some(self.implicit_class_local(scope, 1)),
                    "__annotations__" => Some(self.implicit_class_local(scope, 2)),
                    _ => None,
                }
            } else {
                None
            };
            let md = self.md(scope.m);
            let locals = md.locals.borrow();
            let real = locals.get(&scope.n).and_then(|l| l.get(&name));
            if implicit.is_some() || real.is_some() {
                let mut list: Vec<GNode> = Vec::new();
                list.extend(implicit);
                if let Some(r) = real {
                    list.extend(r.iter().copied());
                }
                self.filter_stmts(node, &list, scope, offset)
            } else {
                Vec::new()
            }
        };
        if !stmts.is_empty() {
            return (scope, stmts.into_iter().map(NV::N).collect());
        }
        // climb out, skipping class scopes
        let mut pscope = self.parent(scope).map(|p| self.scope(p));
        while let Some(ps) = pscope {
            if !self.kind_is(ps, |k| matches!(k, NodeKind::ClassDef(_))) {
                return self.scope_lookup(ps, node, name, 0);
            }
            pscope = self.parent(ps).map(|p| self.scope(p));
        }
        self.builtin_lookup(name)
    }

    /// scoped_nodes/utils.py:17-35 builtin_lookup
    pub fn builtin_lookup(&self, name: GSym) -> LookupResult {
        let bid = self.builtins_mod.get();
        let bmod = GNode {
            m: bid,
            n: NodeId::MODULE,
        };
        if self.sname(name) == "__dict__" {
            return (bmod, Vec::new());
        }
        let md = self.md(bid);
        let locals = md.locals.borrow();
        let stmts = locals
            .get(&NodeId::MODULE)
            .and_then(|l| l.get(&name))
            .cloned()
            .unwrap_or_default();
        (bmod, stmts.into_iter().map(NV::N).collect())
    }

    // ---------- _filter_stmts (filter_statements.py:50-240) ----------

    pub fn filter_stmts(
        &self,
        base_node: GNode,
        stmts: &[GNode],
        frame: GNode,
        offset: i32,
    ) -> Vec<GNode> {
        let myframe = if offset == -1 {
            let f = self.frame(base_node);
            match self.parent(f) {
                Some(p) => self.frame(p),
                None => f,
            }
        } else {
            let mut f = self.frame(base_node);
            // pylint issue #295: node's statement IS its frame (e.g. a
            // default value) -> use the upper frame
            if self.parent(base_node).is_some()
                && self.statement(base_node) == Some(f)
                && self.parent(f).is_some()
            {
                f = self.frame(self.parent(f).unwrap());
            }
            f
        };

        let mystmt: Option<GNode> = if self.parent(base_node).is_some() {
            self.statement(base_node)
        } else {
            None
        };

        let mylineno: u32 = if myframe == frame && mystmt.is_some() {
            let l = self.fromlineno(mystmt.unwrap()) as i64 + offset as i64;
            if l < 0 {
                0
            } else {
                l as u32
            }
        } else {
            0
        };

        let mut _stmts: Vec<GNode> = Vec::new();
        let mut _stmt_parents: Vec<GNode> = Vec::new();

        // _get_filtered_node_statements. Enum-member PROXY placeholders
        // (Instance objects stored directly in class locals by
        // brain_namedtuple_enum) delegate every structural attribute
        // (statement/fromlineno/assign_type/ancestors) to their _proxied
        // fake ClassDef via Instance.__getattr__ — which we reparented
        // into the real tree. Use the proxied class for STRUCTURE, but
        // keep yielding the placeholder itself.
        let mut statements: Vec<(GNode, GNode, GNode)> = stmts
            .iter()
            .filter_map(|&n| {
                let sn = self.proxy_struct(n);
                self.statement(sn).map(|s| (n, sn, s))
            })
            .collect();
        if statements.len() > 1
            && statements
                .iter()
                .all(|(_, _, s)| self.kind_is(*s, |k| matches!(k, NodeKind::ExceptHandler { .. })))
        {
            statements.retain(|(_, _, s)| self.parent_of(*s, base_node));
        }

        for (node, snode, stmt) in statements {
            let stmt_line = self.fromlineno(stmt);
            if stmt_line != 0 && mylineno > 0 && stmt_line > mylineno {
                break;
            }
            // decorator with the same name as the decorated function (#375)
            if mystmt == Some(stmt) && self.is_from_decorator(base_node) {
                continue;
            }
            if self.has_base(snode, base_node) {
                break;
            }

            if self.kind_is(snode, |k| matches!(k, NodeKind::EmptyNode)) {
                _stmts.push(node);
                continue;
            }

            let assign_type = self.assign_type(node);
            // assign_type._get_filtered_stmts
            let md = self.md(assign_type.m);
            let at_kind_filter_base = matches!(
                md.tree.nodes[assign_type.n.idx()].kind,
                NodeKind::FunctionDef(_)
                    | NodeKind::AsyncFunctionDef(_)
                    | NodeKind::ClassDef(_)
                    | NodeKind::Lambda(_)
                    | NodeKind::Import { .. }
                    | NodeKind::ImportFrom { .. }
            );
            let at_is_comprehension = matches!(
                md.tree.nodes[assign_type.n.idx()].kind,
                NodeKind::Comprehension { .. }
            );
            if at_kind_filter_base {
                // _base_nodes.py:93-99
                if self.statement(assign_type) == mystmt {
                    _stmts = vec![node];
                    break;
                }
            } else if at_is_comprehension {
                // node_classes.py:1991-2005 Comprehension._get_filtered_stmts
                if mystmt == Some(assign_type) {
                    let base_is_const_or_name = self.kind_is(base_node, |k| {
                        matches!(k, NodeKind::Const(_) | NodeKind::Name { .. })
                    });
                    if base_is_const_or_name {
                        _stmts = vec![base_node];
                        break;
                    }
                } else if self.statement(assign_type) == mystmt {
                    _stmts = vec![node];
                    break;
                }
            } else {
                // AssignTypeNode._get_filtered_stmts (_base_nodes.py:111-119)
                if Some(assign_type) == mystmt {
                    break;
                }
                if self.statement(assign_type) == mystmt {
                    _stmts = vec![node];
                    break;
                }
            }

            let mut optional_assign = self.optional_assign(assign_type);
            if optional_assign && self.parent_of(assign_type, base_node) {
                // inside a loop, loop var assignment hides previous
                _stmts = vec![node];
                _stmt_parents = vec![self.parent(stmt).unwrap_or(stmt)];
                continue;
            }

            if self.kind_is(assign_type, |k| matches!(k, NodeKind::NamedExpr { .. })) {
                let if_parent = self.get_if_ancestor(assign_type);
                if let Some(ifp) = if_parent {
                    if self.get_if_ancestor(ifp).is_some() {
                        optional_assign = false;
                        _stmts.push(node);
                        _stmt_parents.push(self.parent(stmt).unwrap_or(stmt));
                    } else {
                        _stmts = vec![node];
                        _stmt_parents = vec![self.parent(stmt).unwrap_or(stmt)];
                    }
                } else {
                    _stmts = vec![node];
                    _stmt_parents = vec![self.parent(stmt).unwrap_or(stmt)];
                }
            }

            let stmt_parent = self.parent(stmt).unwrap_or(stmt);
            if let Some(pindex) = _stmt_parents.iter().position(|&p| p == stmt_parent) {
                if self.parent_of(self.assign_type(_stmts[pindex]), assign_type) {
                    // not at the same block level
                    continue;
                }
                if !(optional_assign || self.are_exclusive(_stmts[pindex], node)) {
                    _stmt_parents.remove(pindex);
                    _stmts.remove(pindex);
                }
            }

            if self.are_exclusive(base_node, snode) {
                continue;
            }

            let node_kind_is_assign_name_or_named = self.kind_is(snode, |k| {
                matches!(k, NodeKind::AssignName { .. } | NodeKind::NamedExpr { .. })
            });
            if node_kind_is_assign_name_or_named {
                if self.kind_is(stmt, |k| matches!(k, NodeKind::ExceptHandler { .. })) {
                    if self.parent_of(stmt, base_node) {
                        _stmts = Vec::new();
                        _stmt_parents = Vec::new();
                    } else {
                        continue;
                    }
                } else if !optional_assign
                    && mystmt.is_some()
                    && self.parent(stmt) == self.parent(mystmt.unwrap())
                {
                    _stmts = Vec::new();
                    _stmt_parents = Vec::new();
                }
            } else if self.kind_is(snode, |k| matches!(k, NodeKind::DelName { .. })) {
                _stmts = Vec::new();
                _stmt_parents = Vec::new();
                continue;
            }

            _stmts.push(node);
            let node_is_arguments = self.kind_is(snode, |k| matches!(k, NodeKind::Arguments(_)));
            let parent_is_arguments = self
                .parent(snode)
                .map(|p| self.kind_is(p, |k| matches!(k, NodeKind::Arguments(_))))
                .unwrap_or(false);
            if node_is_arguments || parent_is_arguments {
                _stmt_parents.push(stmt);
            } else {
                _stmt_parents.push(stmt_parent);
            }
        }
        _stmts
    }

    fn is_from_decorator(&self, node: GNode) -> bool {
        let mut cur = node;
        while let Some(p) = self.parent(cur) {
            if self.kind_is(p, |k| matches!(k, NodeKind::Decorators { .. })) {
                return true;
            }
            cur = p;
        }
        false
    }

    fn get_if_ancestor(&self, node: GNode) -> Option<GNode> {
        let mut cur = node;
        while let Some(p) = self.parent(cur) {
            if self.kind_is(p, |k| matches!(k, NodeKind::If { .. })) {
                return Some(p);
            }
            cur = p;
        }
        None
    }

    /// ClassDef.has_base: identity membership in bases
    fn has_base(&self, node: GNode, base: GNode) -> bool {
        if node.m != base.m {
            return false;
        }
        let md = self.md(node.m);
        match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::ClassDef(d) => d.bases.contains(&base.n),
            _ => false,
        }
    }
}
