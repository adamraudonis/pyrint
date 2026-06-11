//! Tree navigation helpers shared by lookup/inference: scope(), frame(),
//! statement(), qname(), are_exclusive(), locate_child(), assign_type().
//! Ports of astroid/nodes/node_ng.py + _base_nodes.py + node_classes.py.

use std::rc::Rc;

use pyast::tree::{Ctx as ExprCtx, NodeKind};
use pyast::NodeId;

use crate::graph::{Engine, Module};
use crate::value::{GNode, ModId};

impl Engine {
    #[inline]
    pub fn md(&self, m: ModId) -> Rc<Module> {
        Rc::clone(&self.mods.borrow()[m.0 as usize])
    }

    pub fn kind_is<F: FnOnce(&NodeKind) -> bool>(&self, g: GNode, f: F) -> bool {
        let md = self.md(g.m);
        f(&md.tree.nodes[g.n.idx()].kind)
    }

    pub fn parent(&self, g: GNode) -> Option<GNode> {
        if g.n == NodeId::MODULE {
            // Module roots normally have no parent — EXCEPT when a
            // register_builtin_transform tip returned the module and
            // _transform_wrapper mutated it permanently
            // (brain_builtin_inference.py:206-210 `if not result.parent:
            // result.parent = node` — e.g. `getattr(self, "x", pickle)`
            // reparents the pickle module under the getattr Call, changing
            // qname() of everything in pickle for the rest of the run).
            let rp = self.reparents.borrow();
            if !rp.is_empty() {
                if let Some(&over) = rp.get(&g) {
                    return Some(over);
                }
            }
            return None;
        }
        let md = self.md(g.m);
        let p = md.tree.nodes[g.n.idx()].parent;
        {
            // reparent overrides apply to ANY node astroid rebinds
            // (`new_call.parent = node.parent` in brain_dataclasses puts a
            // template CALL -- whose tree parent is an Expr -- into the
            // real module's scope chain)
            let rp = self.reparents.borrow();
            if !rp.is_empty() {
                if let Some(&over) = rp.get(&g) {
                    return Some(over);
                }
            }
        }
        Some(GNode { m: g.m, n: p })
    }

    pub fn fromlineno(&self, g: GNode) -> u32 {
        self.md(g.m).tree.nodes[g.n.idx()].fromlineno
    }

    /// node_ng.py:276-285 statement(): nearest enclosing statement incl.
    /// self; None for Module (StatementMissing).
    pub fn statement(&self, g: GNode) -> Option<GNode> {
        let mut cur = g;
        loop {
            if self.is_statement(cur) {
                return Some(cur);
            }
            cur = self.parent(cur)?;
        }
    }

    pub fn is_statement(&self, g: GNode) -> bool {
        let md = self.md(g.m);
        is_statement_kind(&md.tree.nodes[g.n.idx()].kind)
    }

    /// nearest scope node (Module/Func/Lambda/Class/comprehensions),
    /// including self. Decorators get the scope OUTSIDE the function
    /// (node_classes.py Decorators.scope -> parent.parent.scope()).
    pub fn scope(&self, g: GNode) -> GNode {
        let mut cur = g;
        loop {
            {
                let md = self.md(cur.m);
                if matches!(md.tree.nodes[cur.n.idx()].kind, NodeKind::Decorators { .. }) {
                    // Decorators.scope() skips the decorated frame:
                    // parent.parent.scope() — applies whenever the upward
                    // walk REACHES a Decorators node (names inside method
                    // decorators resolve in the class scope)
                    let p = self.parent(cur).unwrap_or(cur);
                    let pp = self.parent(p).unwrap_or(p);
                    return self.scope(pp);
                }
                if matches!(md.tree.nodes[cur.n.idx()].kind, NodeKind::NamedExpr { .. }) {
                    // NamedExpr.scope() (node_classes.py:4940-4957): a
                    // walrus whose parent is Arguments/Keyword/Comprehension
                    // evaluates in the parent's parent scope —
                    // parent.parent.parent.scope() (PEP 572: names inside
                    // `(x := ...)` in a comprehension resolve OUTSIDE the
                    // comprehension scope)
                    if let Some(p) = self.parent(cur) {
                        let pmd = self.md(p.m);
                        if matches!(
                            pmd.tree.nodes[p.n.idx()].kind,
                            NodeKind::Arguments(_)
                                | NodeKind::Keyword { .. }
                                | NodeKind::Comprehension { .. }
                        ) {
                            drop(pmd);
                            let pp = self.parent(p).unwrap_or(p);
                            let ppp = self.parent(pp).unwrap_or(pp);
                            return self.scope(ppp);
                        }
                    }
                }
            }
            if self.is_scope(cur) {
                return cur;
            }
            match self.parent(cur) {
                Some(p) => cur = p,
                None => return cur,
            }
        }
    }

    pub fn is_scope(&self, g: GNode) -> bool {
        let md = self.md(g.m);
        matches!(
            md.tree.nodes[g.n.idx()].kind,
            NodeKind::Module(_)
                | NodeKind::FunctionDef(_)
                | NodeKind::AsyncFunctionDef(_)
                | NodeKind::ClassDef(_)
                | NodeKind::Lambda(_)
                | NodeKind::ListComp(_)
                | NodeKind::SetComp(_)
                | NodeKind::DictComp(_)
                | NodeKind::GeneratorExp(_)
        )
    }

    /// nearest frame (Module/FunctionDef/Lambda/ClassDef), including self.
    pub fn frame(&self, g: GNode) -> GNode {
        let mut cur = g;
        loop {
            if self.is_frame(cur) {
                return cur;
            }
            match self.parent(cur) {
                Some(p) => cur = p,
                None => return cur,
            }
        }
    }

    pub fn is_frame(&self, g: GNode) -> bool {
        let md = self.md(g.m);
        matches!(
            md.tree.nodes[g.n.idx()].kind,
            NodeKind::Module(_)
                | NodeKind::FunctionDef(_)
                | NodeKind::AsyncFunctionDef(_)
                | NodeKind::ClassDef(_)
                | NodeKind::Lambda(_)
        )
    }

    pub fn is_comprehension_scope(&self, g: GNode) -> bool {
        let md = self.md(g.m);
        matches!(
            md.tree.nodes[g.n.idx()].kind,
            NodeKind::ListComp(_) | NodeKind::SetComp(_) | NodeKind::DictComp(_) | NodeKind::GeneratorExp(_)
        )
    }

    /// True if `anc` is a (strict) ancestor of `node` (node_ng parent_of).
    pub fn parent_of(&self, anc: GNode, node: GNode) -> bool {
        let mut cur = node;
        while let Some(p) = self.parent(cur) {
            if p == anc {
                return true;
            }
            cur = p;
        }
        false
    }

    /// LocalsDictNodeNG.qname (mixin.py:40-50): chain of names through
    /// parent frames up to the module.
    pub fn qname(&self, g: GNode) -> String {
        let md = self.md(g.m);
        if let Some(qn) = md.qnames.get(&g.n) {
            return qn.clone();
        }
        let mut parts: Vec<String> = Vec::new();
        let mut cur = g;
        loop {
            // re-fetch per iteration: reparented nodes cross module trees
            let cmd = self.md(cur.m);
            let name = match &cmd.tree.nodes[cur.n.idx()].kind {
                NodeKind::Module(d) => {
                    parts.push(d.name.to_string());
                    // `if self.parent is None: return self.name` — a module
                    // REPARENTED by _transform_wrapper
                    // (brain_builtin_inference.py:206-210) keeps walking:
                    // qname = parent.frame().qname() + "." + name
                    match self.parent(cur) {
                        Some(p) => {
                            cur = p;
                            continue;
                        }
                        None => break,
                    }
                }
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                    cmd.tree.s(d.name).to_string()
                }
                NodeKind::ClassDef(d) => cmd.tree.s(d.name).to_string(),
                NodeKind::Lambda(_) => "<lambda>".to_string(),
                _ => {
                    // shouldn't happen for qname targets; fall to parent
                    match self.parent(cur) {
                        Some(p) => {
                            cur = p;
                            continue;
                        }
                        None => break,
                    }
                }
            };
            parts.push(name);
            match self.parent(cur) {
                Some(p) => cur = self.frame(p),
                None => break,
            }
        }
        parts.reverse();
        parts.join(".")
    }

    pub fn node_name(&self, g: GNode) -> Option<String> {
        let md = self.md(g.m);
        match &md.tree.nodes[g.n.idx()].kind {
            NodeKind::Module(d) => Some(d.name.to_string()),
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                Some(md.tree.s(d.name).to_string())
            }
            NodeKind::ClassDef(d) => Some(md.tree.s(d.name).to_string()),
            NodeKind::Name { name } | NodeKind::AssignName { name } | NodeKind::DelName { name } => {
                Some(md.tree.s(*name).to_string())
            }
            NodeKind::Attribute { attrname, .. } | NodeKind::AssignAttr { attrname, .. } => {
                Some(md.tree.s(*attrname).to_string())
            }
            NodeKind::Lambda(_) => Some("<lambda>".to_string()),
            _ => None,
        }
    }

    /// PROXY placeholders (Instance objects stored in locals, e.g. enum
    /// members) delegate structural attributes to their _proxied class
    /// via Instance.__getattr__ (bases.py Proxy). Returns the proxied
    /// ClassDef for such placeholders, the node itself otherwise.
    pub fn proxy_struct(&self, g: GNode) -> GNode {
        if self.proxy_placeholders.borrow().contains(&g) {
            if let Some(crate::value::NV::V(
                crate::value::Value::Inst { cls, .. } | crate::value::Value::ExcInst { cls, .. },
            )) = self.redirects.borrow().get(&g)
            {
                return *cls;
            }
        }
        g
    }

    /// _base_nodes.py:90-126 + node_classes assign_type dispatch.
    /// Proxy placeholders delegate to their _proxied class (Instance
    /// assign_type -> ClassDef.assign_type -> self).
    pub fn assign_type(&self, g: GNode) -> GNode {
        let g = self.proxy_struct(g);
        let md = self.md(g.m);
        match &md.tree.nodes[g.n.idx()].kind {
            // FilterStmtsBaseNode + AssignTypeNode -> self
            NodeKind::FunctionDef(_)
            | NodeKind::AsyncFunctionDef(_)
            | NodeKind::ClassDef(_)
            | NodeKind::Lambda(_)
            | NodeKind::Import { .. }
            | NodeKind::ImportFrom { .. }
            | NodeKind::Assign { .. }
            | NodeKind::AnnAssign { .. }
            | NodeKind::AugAssign { .. }
            | NodeKind::Delete { .. }
            | NodeKind::For(_)
            | NodeKind::AsyncFor(_)
            | NodeKind::With(_)
            | NodeKind::AsyncWith(_)
            | NodeKind::ExceptHandler { .. }
            | NodeKind::NamedExpr { .. }
            | NodeKind::MatchAs { .. }
            | NodeKind::MatchStar { .. }
            | NodeKind::MatchMapping { .. }
            | NodeKind::TypeAlias { .. }
            | NodeKind::TypeVar { .. }
            | NodeKind::TypeVarTuple { .. }
            | NodeKind::ParamSpec { .. }
            | NodeKind::Comprehension { .. }
            | NodeKind::Arguments(_) => g,
            // ParentAssignNode -> parent.assign_type()
            NodeKind::AssignName { .. }
            | NodeKind::AssignAttr { .. }
            | NodeKind::DelName { .. }
            | NodeKind::DelAttr { .. }
            | NodeKind::Starred { .. } => {
                let p = self.parent(g).unwrap_or(g);
                self.assign_type(p)
            }
            NodeKind::Tuple { ctx, .. } | NodeKind::List { ctx, .. }
                if *ctx == ExprCtx::Store =>
            {
                let p = self.parent(g).unwrap_or(g);
                self.assign_type(p)
            }
            _ => {
                // NodeNG has no assign_type; reaching here would be an
                // AttributeError in astroid. Be conservative: self.
                g
            }
        }
    }

    /// `For`/`AsyncFor`/`Comprehension`.optional_assign
    pub fn optional_assign(&self, g: GNode) -> bool {
        let md = self.md(g.m);
        matches!(
            md.tree.nodes[g.n.idx()].kind,
            NodeKind::For(_) | NodeKind::AsyncFor(_) | NodeKind::Comprehension { .. }
        )
    }

    /// NodeNG.locate_child equivalent: which field of `parent` contains
    /// `child` (directly). Returns the astroid field name.
    pub fn locate_child(&self, parent: GNode, child: GNode) -> Option<&'static str> {
        debug_assert_eq!(parent.m, child.m);
        let md = self.md(parent.m);
        let c = child.n;
        let kind = &md.tree.nodes[parent.n.idx()].kind;
        match kind {
            NodeKind::If { test, body, orelse } => {
                if *test == c {
                    Some("test")
                } else if body.contains(&c) {
                    Some("body")
                } else if orelse.contains(&c) {
                    Some("orelse")
                } else {
                    None
                }
            }
            NodeKind::IfExp { test, body, orelse } => {
                if *test == c {
                    Some("test")
                } else if *body == c {
                    Some("body")
                } else if *orelse == c {
                    Some("orelse")
                } else {
                    None
                }
            }
            NodeKind::Try(d) | NodeKind::TryStar(d) => {
                if d.body.contains(&c) {
                    Some("body")
                } else if d.handlers.contains(&c) {
                    Some("handlers")
                } else if d.orelse.contains(&c) {
                    Some("orelse")
                } else if d.finalbody.contains(&c) {
                    Some("finalbody")
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// node_classes.py:116-186 are_exclusive (exceptions=None form).
    pub fn are_exclusive(&self, stmt1: GNode, stmt2: GNode) -> bool {
        // index stmt1's parents
        let mut stmt1_parents: rustc_hash::FxHashMap<GNode, GNode> = Default::default();
        let mut previous = stmt1;
        let mut cur = stmt1;
        while let Some(node) = self.parent(cur) {
            stmt1_parents.insert(node, previous);
            previous = node;
            cur = node;
        }
        // climb stmt2's parents to the first common one
        let mut previous2 = stmt2;
        let mut cur2 = stmt2;
        while let Some(node) = self.parent(cur2) {
            if let Some(&child1) = stmt1_parents.get(&node) {
                let md = self.md(node.m);
                let k = &md.tree.nodes[node.n.idx()].kind;
                if matches!(k, NodeKind::If { .. }) {
                    let c2attr = self.locate_child(node, previous2);
                    let c1attr = self.locate_child(node, child1);
                    if c1attr == Some("test") || c2attr == Some("test") {
                        return false;
                    }
                    if c1attr != c2attr {
                        return true;
                    }
                } else if matches!(k, NodeKind::Try(_)) {
                    let c2 = self.locate_child(node, previous2);
                    let c1 = self.locate_child(node, child1);
                    if previous2 != child1 {
                        // exceptions=None: ExceptHandler.catch(None) is True
                        let first_in_body_caught = c2 == Some("handlers") && c1 == Some("body");
                        let second_in_body_caught = c2 == Some("body") && c1 == Some("handlers");
                        let first_in_else = c2 == Some("handlers") && c1 == Some("orelse");
                        let second_in_else = c2 == Some("orelse") && c1 == Some("handlers");
                        if first_in_body_caught
                            || second_in_body_caught
                            || first_in_else
                            || second_in_else
                        {
                            return true;
                        }
                    } else if c2 == Some("handlers") && c1 == Some("handlers") {
                        return previous2 != child1;
                    }
                }
                return false;
            }
            previous2 = node;
            cur2 = node;
        }
        false
    }

    /// preorder walk collecting node ids (children() order).
    pub fn walk_preorder(&self, m: ModId) -> Vec<NodeId> {
        let md = self.md(m);
        let mut out = Vec::with_capacity(md.tree.nodes.len());
        let mut stack = vec![NodeId::MODULE];
        let mut buf = Vec::new();
        while let Some(id) = stack.pop() {
            out.push(id);
            buf.clear();
            md.tree.push_children(id, &mut buf);
            for &c in buf.iter().rev() {
                stack.push(c);
            }
        }
        out
    }
}

pub fn is_statement_kind(k: &NodeKind) -> bool {
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

pub fn kind_label(k: &NodeKind) -> &'static str {
    match k {
        NodeKind::Module(_) => "Module",
        NodeKind::FunctionDef(_) => "FunctionDef",
        NodeKind::AsyncFunctionDef(_) => "AsyncFunctionDef",
        NodeKind::ClassDef(_) => "ClassDef",
        NodeKind::Lambda(_) => "Lambda",
        NodeKind::Const(_) => "Const",
        NodeKind::Name { .. } => "Name",
        NodeKind::AssignName { .. } => "AssignName",
        NodeKind::Attribute { .. } => "Attribute",
        NodeKind::Call { .. } => "Call",
        NodeKind::Import { .. } => "Import",
        NodeKind::ImportFrom { .. } => "ImportFrom",
        NodeKind::Subscript { .. } => "Subscript",
        NodeKind::Arguments(_) => "Arguments",
        NodeKind::Tuple { .. } => "Tuple",
        NodeKind::List { .. } => "List",
        NodeKind::Set { .. } => "Set",
        NodeKind::Dict { .. } => "Dict",
        NodeKind::BinOp { .. } => "BinOp",
        NodeKind::BoolOp { .. } => "BoolOp",
        NodeKind::UnaryOp { .. } => "UnaryOp",
        NodeKind::Compare { .. } => "Compare",
        NodeKind::IfExp { .. } => "IfExp",
        NodeKind::AugAssign { .. } => "AugAssign",
        NodeKind::AssignAttr { .. } => "AssignAttr",
        NodeKind::Slice { .. } => "Slice",
        NodeKind::Starred { .. } => "Starred",
        NodeKind::NamedExpr { .. } => "NamedExpr",
        NodeKind::JoinedStr { .. } => "JoinedStr",
        NodeKind::FormattedValue { .. } => "FormattedValue",
        NodeKind::ListComp(_) => "ListComp",
        NodeKind::SetComp(_) => "SetComp",
        NodeKind::DictComp(_) => "DictComp",
        NodeKind::GeneratorExp(_) => "GeneratorExp",
        NodeKind::EmptyNode => "EmptyNode",
        NodeKind::Unknown => "Unknown",
        NodeKind::Global { .. } => "Global",
        NodeKind::DelName { .. } => "DelName",
        _ => "Node",
    }
}
