//! MisdesignChecker port (pylint/checkers/design_analysis.py): R0901-R0917.
//!
//! Pure AST checker, name="design". Hooks into the walk:
//! - visit_classdef  -> R0901 (too-many-ancestors), R0902 (too-many-instance-attributes)
//! - leave_classdef  -> R0904 (too-many-public-methods), R0903 (too-few-public-methods)
//! - visit_functiondef (sync+async) -> R0913 (too-many-arguments), R0917
//!   (too-many-positional-arguments), R0914 (too-many-locals) + push return/stmt frames
//! - leave_functiondef -> R0911 (too-many-return-statements), R0912 (too-many-branches),
//!   R0915 (too-many-statements)
//! - visit_return -> count returns
//! - visit_if  -> branches (+R0916 too-many-boolean-expressions) + stmts
//! - visit_try -> branches + stmts
//! - visit_while/visit_for -> branches only
//! - visit_match -> +1 stmt + branches
//! - visit_default -> +1 stmt for is_statement nodes WITHOUT a registered
//!   specific design visitor (the R0915 coupling, notes/09-design §A.2.1).
//!
//! The walker calls these only when the relevant @only_required_for_messages
//! gate is satisfied (Prepared.design_*). visit_default is special: it is
//! registered for every node class for which the checker did NOT register an
//! enabled specific visitor (ast_walker.py:64-69). We replicate that by
//! letting the walker decide per-node whether the design specific-visitor ran
//! (and is enabled); if not, and the checker is prepared, visit_default runs.

use pyast::tree::NodeKind;
use pyinfer::value::GNode;

use crate::ckutils as u;
use crate::walker::WalkCx;

// ---- config defaults (design_analysis.py options block) ----
const MAX_ARGS: i64 = 5;
const MAX_POSITIONAL_ARGS: i64 = 5;
const MAX_LOCALS: i64 = 15;
const MAX_RETURNS: i64 = 6;
const MAX_BRANCHES: i64 = 12;
const MAX_STATEMENTS: i64 = 50;
const MAX_PARENTS: i64 = 7;
const MAX_ATTRIBUTES: i64 = 7;
const MIN_PUBLIC_METHODS: i64 = 2;
const MAX_PUBLIC_METHODS: i64 = 20;
const MAX_BOOL_EXPR: i64 = 5;

/// STDLIB_CLASSES_IGNORE_ANCESTOR (design_analysis.py:100-181) — kept
/// VERBATIM including the `bulitins.frozenset` typo so builtins.frozenset is
/// NOT ignored.
const STDLIB_CLASSES_IGNORE_ANCESTOR: &[&str] = &[
    "builtins.object",
    "builtins.tuple",
    "builtins.dict",
    "builtins.list",
    "builtins.set",
    "bulitins.frozenset",
    "collections.ChainMap",
    "collections.Counter",
    "collections.OrderedDict",
    "collections.UserDict",
    "collections.UserList",
    "collections.UserString",
    "collections.defaultdict",
    "collections.deque",
    "collections.namedtuple",
    "_collections_abc.Awaitable",
    "_collections_abc.Coroutine",
    "_collections_abc.AsyncIterable",
    "_collections_abc.AsyncIterator",
    "_collections_abc.AsyncGenerator",
    "_collections_abc.Hashable",
    "_collections_abc.Iterable",
    "_collections_abc.Iterator",
    "_collections_abc.Generator",
    "_collections_abc.Reversible",
    "_collections_abc.Sized",
    "_collections_abc.Container",
    "_collections_abc.Collection",
    "_collections_abc.Set",
    "_collections_abc.MutableSet",
    "_collections_abc.Mapping",
    "_collections_abc.MutableMapping",
    "_collections_abc.MappingView",
    "_collections_abc.KeysView",
    "_collections_abc.ItemsView",
    "_collections_abc.ValuesView",
    "_collections_abc.Sequence",
    "_collections_abc.MutableSequence",
    "_collections_abc.ByteString",
    "typing.Tuple",
    "typing.List",
    "typing.Dict",
    "typing.Set",
    "typing.FrozenSet",
    "typing.Deque",
    "typing.DefaultDict",
    "typing.OrderedDict",
    "typing.Counter",
    "typing.ChainMap",
    "typing.Awaitable",
    "typing.Coroutine",
    "typing.AsyncIterable",
    "typing.AsyncIterator",
    "typing.AsyncGenerator",
    "typing.Iterable",
    "typing.Iterator",
    "typing.Generator",
    "typing.Reversible",
    "typing.Container",
    "typing.Collection",
    "typing.AbstractSet",
    "typing.MutableSet",
    "typing.Mapping",
    "typing.MutableMapping",
    "typing.Sequence",
    "typing.MutableSequence",
    "typing.ByteString",
    "typing.MappingView",
    "typing.KeysView",
    "typing.ItemsView",
    "typing.ValuesView",
    "typing.ContextManager",
    "typing.AsyncContextManager",
    "typing.Hashable",
    "typing.Sized",
    "typing.NamedTuple",
    "typing.TypedDict",
    "typing_extensions.TypedDict",
];

const TYPING_NAMEDTUPLE: &str = "typing.NamedTuple";
const TYPING_TYPEDDICT: &str = "typing.TypedDict";
const TYPING_EXTENSIONS_TYPEDDICT: &str = "typing_extensions.TypedDict";

/// SPECIAL_OBJ = re.compile("^_{2}[a-z]+_{2}$") — exactly two leading and
/// trailing underscores around one-or-more lowercase ascii letters.
fn special_obj_match(name: &str) -> bool {
    let b = name.as_bytes();
    // ^__   ...   __$
    if b.len() < 5 {
        return false;
    }
    if !(b[0] == b'_' && b[1] == b'_') {
        return false;
    }
    let n = b.len();
    if !(b[n - 1] == b'_' && b[n - 2] == b'_') {
        return false;
    }
    // middle must be one-or-more [a-z]
    let mid = &b[2..n - 2];
    if mid.is_empty() {
        return false;
    }
    mid.iter().all(|c| c.is_ascii_lowercase())
}

/// MisdesignChecker run state (lives in LintRun, persists across modules —
/// the stacks naturally empty at module end since the walk is balanced; open()
/// resets are per-RUN, and pylint runs open() once at run start).
#[derive(Default)]
pub struct DesignCk {
    /// stack of statement counters, one per currently open FunctionDef
    /// (pushed at visit, popped at leave). is_statement nodes increment ALL
    /// open frames (_inc_all_stmts) -> nested-function statement leakage.
    stmts: Vec<i64>,
    /// stack of return counters, one per open FunctionDef.
    returns: Vec<i64>,
    /// _branches: defaultdict keyed by scope node -> branch count.
    branches: rustc_hash::FxHashMap<GNode, i64>,
}

impl DesignCk {
    fn inc_all_stmts(&mut self, amount: i64) {
        for s in self.stmts.iter_mut() {
            *s += amount;
        }
    }

    fn inc_branch(&mut self, cx: &WalkCx, node: GNode, n: i64) {
        let sc = cx.eng.scope(node);
        *self.branches.entry(sc).or_insert(0) += n;
    }

    /// visit_default — increments statements for is_statement nodes. The
    /// walker calls this only for node classes where the design checker did
    /// NOT register an (enabled) specific visitor.
    pub fn visit_default_stmt(&mut self, kind: &NodeKind) {
        if pyinfer::treeutil::is_statement_kind(kind) {
            self.inc_all_stmts(1);
        }
    }

    /// visit_classdef (design_analysis.py:447-481) — R0901, R0902.
    /// `gate_r0901` etc. tell us which of the four messages are enabled.
    pub fn visit_classdef(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        gate_r0901: bool,
        gate_r0902: bool,
    ) {
        if gate_r0901 {
            // _get_parents: work-list DFS over ancestors(recurs=False),
            // skipping ignored qnames (and their ancestors), dedup by node.
            let nb_parents = self.count_parents(cx, node);
            if nb_parents as i64 > MAX_PARENTS {
                cx.emit_node(
                    "R0901",
                    u::msg_line(cx.eng, node),
                    u::msg_col(cx.eng, node),
                    format!("Too many ancestors ({nb_parents}/{MAX_PARENTS})"),
                );
            }
        }
        if gate_r0902 {
            // filtered_attrs = [k for (k,v) in instance_attrs.items()
            //                   if v[0].root() is root]
            let root = cx.eng.root(node);
            let iattrs = cx.eng.instance_attrs_of(node);
            let mut count = 0usize;
            for (_k, v) in iattrs.iter() {
                if let Some(&first) = v.first() {
                    if cx.eng.root(first) == root {
                        count += 1;
                    }
                }
            }
            if count as i64 > MAX_ATTRIBUTES {
                cx.emit_node(
                    "R0902",
                    u::msg_line(cx.eng, node),
                    u::msg_col(cx.eng, node),
                    format!("Too many instance attributes ({count}/{MAX_ATTRIBUTES})"),
                );
            }
        }
    }

    /// _get_parents_iter count (design_analysis.py:246-279).
    fn count_parents(&self, cx: &WalkCx, node: GNode) -> usize {
        let mut parents: rustc_hash::FxHashSet<GNode> = Default::default();
        // to_explore = list(node.ancestors(recurs=False)); LIFO pop.
        let mut to_explore: Vec<GNode> = cx.eng.ancestors(node, false, None);
        while let Some(parent) = to_explore.pop() {
            let qn = cx.eng.qname(parent);
            if STDLIB_CLASSES_IGNORE_ANCESTOR.contains(&qn.as_str()) {
                // ignored-parents default is () so only the stdlib set applies
                continue;
            }
            if parents.insert(parent) {
                // yield + explore its bases
                to_explore.extend(cx.eng.ancestors(parent, false, None));
            }
        }
        parents.len()
    }

    /// leave_classdef (design_analysis.py:483-527) — R0904 then R0903.
    pub fn leave_classdef(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        gate_r0904: bool,
        gate_r0903: bool,
    ) {
        // my_methods = count of mymethods() whose name has no leading "_"
        if gate_r0904 {
            let my_methods = self
                .mymethods(cx, node)
                .iter()
                .filter(|&&m| !cx.eng.node_name(m).is_some_and(|n| n.starts_with('_')))
                .count();
            if my_methods as i64 > MAX_PUBLIC_METHODS {
                cx.emit_node(
                    "R0904",
                    u::msg_line(cx.eng, node),
                    u::msg_col(cx.eng, node),
                    format!("Too many public methods ({my_methods}/{MAX_PUBLIC_METHODS})"),
                );
            }
        }

        if !gate_r0903 {
            return;
        }
        // exclude-too-few-public-methods default [] -> exclusion #1 skipped.
        // exclusion #2: if node.type != "class" or _is_exempt: return
        if cx.eng.class_type(node) != "class" || self.is_exempt_from_public_methods(cx, node) {
            return;
        }
        let all_methods = self.count_methods_in_class(cx, node);
        if all_methods < MIN_PUBLIC_METHODS {
            cx.emit_node(
                "R0903",
                u::msg_line(cx.eng, node),
                u::msg_col(cx.eng, node),
                format!("Too few public methods ({all_methods}/{MIN_PUBLIC_METHODS})"),
            );
        }
    }

    /// ClassDef.mymethods() (scoped_nodes.py:2606-2614): for each name in
    /// locals (insertion order), take the FIRST binding; keep it if it is a
    /// FunctionDef/AsyncFunctionDef.
    fn mymethods(&self, cx: &WalkCx, node: GNode) -> Vec<GNode> {
        let md = cx.eng.md(node.m);
        let locals = md.locals.borrow();
        let mut out = Vec::new();
        if let Some(scope_locals) = locals.get(&node.n) {
            for (name, bindings) in scope_locals.iter() {
                // astroid injects __module__/__qualname__ (Const) and
                // __annotations__ (Dict) as the FIRST class-local binding at
                // class creation (ClassDef.implicit_locals); our locals map
                // omits the synthetic node (getattr injects it lazily), so a
                // user `def __module__` would otherwise be the first binding.
                // values()[0] for these names is ALWAYS the synthetic
                // non-FunctionDef -> they never appear in mymethods().
                let nm = cx.eng.sname(*name);
                if nm == "__module__" || nm == "__qualname__" || nm == "__annotations__" {
                    continue;
                }
                if let Some(&first) = bindings.first() {
                    let fmd = cx.eng.md(first.m);
                    if matches!(
                        fmd.tree.nodes[first.n.idx()].kind,
                        NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)
                    ) {
                        out.push(first);
                    }
                }
            }
        }
        out
    }

    /// ClassDef.methods() (scoped_nodes.py:2592-2604): self + recursive
    /// ancestors; for each, its mymethods() members whose NAME hasn't been
    /// seen (first definition wins).
    fn methods(&self, cx: &WalkCx, node: GNode) -> Vec<GNode> {
        let mut seen: rustc_hash::FxHashSet<String> = Default::default();
        let mut out = Vec::new();
        let mut classes = vec![node];
        classes.extend(cx.eng.ancestors(node, true, None));
        for cls in classes {
            for m in self.mymethods(cx, cls) {
                if let Some(name) = cx.eng.node_name(m) {
                    if seen.insert(name) {
                        out.push(m);
                    }
                }
            }
        }
        out
    }

    /// _count_methods_in_class (design_analysis.py:236-243).
    fn count_methods_in_class(&self, cx: &WalkCx, node: GNode) -> i64 {
        let mut all_methods = self
            .methods(cx, node)
            .iter()
            .filter(|&&m| !cx.eng.node_name(m).is_some_and(|n| n.starts_with('_')))
            .count() as i64;
        for m in self.mymethods(cx, node) {
            if let Some(name) = cx.eng.node_name(m) {
                if special_obj_match(&name) && name != "__init__" {
                    all_methods += 1;
                }
            }
        }
        all_methods
    }

    /// _is_exempt_from_public_methods (design_analysis.py:184-219).
    fn is_exempt_from_public_methods(&self, cx: &WalkCx, node: GNode) -> bool {
        for ancestor in cx.eng.ancestors(node, true, None) {
            // is_enum: ancestor.name == "Enum" and ancestor.root().name == "enum"
            if cx.eng.node_name(ancestor).as_deref() == Some("Enum") {
                let root = cx.eng.root(ancestor);
                if cx.eng.node_name(root).as_deref() == Some("enum") {
                    return true;
                }
            }
            let qn = cx.eng.qname(ancestor);
            if qn == TYPING_NAMEDTUPLE
                || qn == TYPING_TYPEDDICT
                || qn == TYPING_EXTENSIONS_TYPEDDICT
            {
                return true;
            }
        }
        // dataclass / attrs decorator name heuristic
        let md = cx.eng.md(node.m);
        let decorators = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::ClassDef(d) => d.decorators,
            _ => None,
        };
        let Some(dec_node) = decorators else {
            return false;
        };
        let dec_children: Vec<pyast::NodeId> = match &md.tree.nodes[dec_node.idx()].kind {
            NodeKind::Decorators { nodes } => nodes.clone(),
            _ => Vec::new(),
        };
        // root_locals = set(node.root().locals)
        let root = cx.eng.root(node);
        let root_md = cx.eng.md(root.m);
        let root_local_names: rustc_hash::FxHashSet<String> = {
            let locals = root_md.locals.borrow();
            locals
                .get(&root.n)
                .map(|l| l.keys().map(|k| cx.eng.sname(*k)).collect())
                .unwrap_or_default()
        };
        let has = |names: &[&str]| names.iter().any(|n| root_local_names.contains(*n));
        for dec in dec_children {
            // unwrap Call.func
            let dmd = cx.eng.md(node.m);
            let inner = match &dmd.tree.nodes[dec.idx()].kind {
                NodeKind::Call { func, .. } => *func,
                _ => dec,
            };
            let name = match &dmd.tree.nodes[inner.idx()].kind {
                NodeKind::Name { name } => dmd.tree.s(*name).to_string(),
                NodeKind::Attribute { attrname, .. } => dmd.tree.s(*attrname).to_string(),
                _ => continue,
            };
            // DATACLASSES_DECORATORS = {dataclass, attrs}; DATACLASS_IMPORT = dataclasses
            if (name == "dataclass" || name == "attrs")
                && (has(&["dataclass", "attrs"]) || root_local_names.contains("dataclasses"))
            {
                return true;
            }
            // ATTRS_DECORATORS = {define, frozen}; ATTRS_IMPORT = attrs
            if (name == "define" || name == "frozen")
                && (has(&["define", "frozen"]) || root_local_names.contains("attrs"))
            {
                return true;
            }
        }
        false
    }

    /// visit_functiondef / visit_asyncfunctiondef (design_analysis.py:538-596)
    /// — R0913, R0917, R0914; pushes the return + statement frames.
    pub fn visit_functiondef(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        gate_r0913: bool,
        gate_r0917: bool,
        gate_r0914: bool,
    ) {
        self.returns.push(0);

        let md = cx.eng.md(node.m);
        let args_node = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.args,
            _ => {
                self.stmts.push(1);
                return;
            }
        };
        let (n_pos, n_args_args, n_kwonly, pos_names, kwonly_names) =
            match &md.tree.nodes[args_node.idx()].kind {
                NodeKind::Arguments(a) => {
                    // args = args + posonlyargs + kwonlyargs
                    // pos_args = args + posonlyargs
                    let arg_name = |id: &pyast::NodeId| -> String {
                        match &md.tree.nodes[id.idx()].kind {
                            NodeKind::AssignName { name } => md.tree.s(*name).to_string(),
                            _ => String::new(),
                        }
                    };
                    let pos_names: Vec<String> = a
                        .args
                        .iter()
                        .chain(a.posonlyargs.iter())
                        .map(arg_name)
                        .collect();
                    let kwonly_names: Vec<String> = a.kwonlyargs.iter().map(arg_name).collect();
                    (
                        a.posonlyargs.len(),
                        a.args.len(),
                        a.kwonlyargs.len(),
                        pos_names,
                        kwonly_names,
                    )
                }
                _ => (0, 0, 0, Vec::new(), Vec::new()),
            };
        // len(args) = args.args + posonlyargs + kwonlyargs
        let len_args = (n_args_args + n_pos + n_kwonly) as i64;

        // ignored-argument-names default: re.compile("_.*|^ignored_|^unused_")
        let ignored = |name: &str| -> bool {
            name.starts_with('_') || name.starts_with("ignored_") || name.starts_with("unused_")
        };
        let ignored_pos_args_num = pos_names.iter().filter(|n| ignored(n)).count() as i64;
        let ignored_kwonly = kwonly_names.iter().filter(|n| ignored(n)).count() as i64;
        let ignored_args_num = ignored_pos_args_num + ignored_kwonly;

        // R0913: compare argnum (ignored excluded), report len(args) (bug)
        if gate_r0913 {
            let argnum = len_args - ignored_args_num;
            if argnum > MAX_ARGS {
                cx.emit_node(
                    "R0913",
                    u::msg_line(cx.eng, node),
                    u::msg_col(cx.eng, node),
                    format!("Too many arguments ({len_args}/{MAX_ARGS})"),
                );
            }
        }
        // R0917: pos_args_count = len(args) - len(kwonlyargs) - ignored_pos
        if gate_r0917 {
            let pos_args_count = len_args - n_kwonly as i64 - ignored_pos_args_num;
            if pos_args_count > MAX_POSITIONAL_ARGS {
                cx.emit_node(
                    "R0917",
                    u::msg_line(cx.eng, node),
                    u::msg_col(cx.eng, node),
                    format!(
                        "Too many positional arguments ({pos_args_count}/{MAX_POSITIONAL_ARGS})"
                    ),
                );
            }
        }
        // R0914: locnum = len(node.locals) - ignored_args_num; -1 if "_" local
        if gate_r0914 {
            let (n_locals, has_underscore) = {
                let md2 = cx.eng.md(node.m);
                let locals = md2.locals.borrow();
                match locals.get(&node.n) {
                    Some(l) => (
                        l.len() as i64,
                        l.keys().any(|k| cx.eng.sname(*k) == "_"),
                    ),
                    None => (0, false),
                }
            };
            let mut locnum = n_locals - ignored_args_num;
            if has_underscore {
                locnum -= 1;
            }
            if locnum > MAX_LOCALS {
                cx.emit_node(
                    "R0914",
                    u::msg_line(cx.eng, node),
                    u::msg_col(cx.eng, node),
                    format!("Too many local variables ({locnum}/{MAX_LOCALS})"),
                );
            }
        }
        self.stmts.push(1);
    }

    /// leave_functiondef (design_analysis.py:605-632) — R0911, R0912, R0915.
    pub fn leave_functiondef(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        gate_r0911: bool,
        gate_r0912: bool,
        gate_r0915: bool,
    ) {
        let returns = self.returns.pop().unwrap_or(0);
        if gate_r0911 && returns > MAX_RETURNS {
            cx.emit_node(
                "R0911",
                u::msg_line(cx.eng, node),
                u::msg_col(cx.eng, node),
                format!("Too many return statements ({returns}/{MAX_RETURNS})"),
            );
        }
        let branches = self.branches.get(&node).copied().unwrap_or(0);
        if gate_r0912 && branches > MAX_BRANCHES {
            cx.emit_node(
                "R0912",
                u::msg_line(cx.eng, node),
                u::msg_col(cx.eng, node),
                format!("Too many branches ({branches}/{MAX_BRANCHES})"),
            );
        }
        let stmts = self.stmts.pop().unwrap_or(0);
        if gate_r0915 && stmts > MAX_STATEMENTS {
            cx.emit_node(
                "R0915",
                u::msg_line(cx.eng, node),
                u::msg_col(cx.eng, node),
                format!("Too many statements ({stmts}/{MAX_STATEMENTS})"),
            );
        }
    }

    /// visit_return (ungated): count returns into the current frame.
    pub fn visit_return(&mut self) {
        if let Some(last) = self.returns.last_mut() {
            *last += 1;
        }
    }

    /// visit_try (ungated): branches = handlers + bool(orelse) + bool(finalbody),
    /// incremented as both branch and statement count.
    pub fn visit_try(&mut self, cx: &mut WalkCx, node: GNode) {
        let md = cx.eng.md(node.m);
        let branches = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Try(d) | NodeKind::TryStar(d) => {
                d.handlers.len() as i64
                    + i64::from(!d.orelse.is_empty())
                    + i64::from(!d.finalbody.is_empty())
            }
            _ => 0,
        };
        self.inc_branch(cx, node, branches);
        self.inc_all_stmts(branches);
    }

    /// visit_if (gated R0916|R0912): boolean-expr check + branches + stmts.
    pub fn visit_if(&mut self, cx: &mut WalkCx, node: GNode, gate_r0916: bool) {
        if gate_r0916 {
            self.check_boolean_expressions(cx, node);
        }
        let md = cx.eng.md(node.m);
        let mut branches = 1i64;
        if let NodeKind::If { orelse, .. } = &md.tree.nodes[node.n.idx()].kind {
            // don't double count If from elif: orelse is exactly one If
            let is_elif = orelse.len() == 1
                && matches!(md.tree.nodes[orelse[0].idx()].kind, NodeKind::If { .. });
            if !orelse.is_empty() && !is_elif {
                branches += 1;
            }
        }
        self.inc_branch(cx, node, branches);
        self.inc_all_stmts(branches);
    }

    /// _check_boolean_expressions (design_analysis.py:670-683) — R0916. Report
    /// node is the BoolOp condition (no `position` -> fromlineno/col_offset).
    fn check_boolean_expressions(&mut self, cx: &mut WalkCx, node: GNode) {
        let md = cx.eng.md(node.m);
        let test = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::If { test, .. } => *test,
            _ => return,
        };
        let cond = GNode { m: node.m, n: test };
        if !matches!(md.tree.nodes[test.idx()].kind, NodeKind::BoolOp { .. }) {
            return;
        }
        let nb = count_boolean_expressions(cx, cond);
        if nb > MAX_BOOL_EXPR {
            // BoolOp has no astroid `position` -> fromlineno / col_offset
            let line = u::lineno(cx.eng, cond);
            let col = (u::col_offset(cx.eng, cond) as i64).max(0);
            cx.emit_node(
                "R0916",
                line,
                col,
                format!("Too many boolean expressions in if statement ({nb}/{MAX_BOOL_EXPR})"),
            );
        }
    }

    /// visit_while / visit_for (ungated): branches = 1 + bool(orelse); NO stmt
    /// increment.
    pub fn visit_while_or_for(&mut self, cx: &mut WalkCx, node: GNode) {
        let md = cx.eng.md(node.m);
        let has_orelse = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::While { orelse, .. } => !orelse.is_empty(),
            NodeKind::For(d) | NodeKind::AsyncFor(d) => !d.orelse.is_empty(),
            _ => false,
        };
        let branches = 1 + i64::from(has_orelse);
        self.inc_branch(cx, node, branches);
    }

    /// visit_match (ungated): +1 stmt; branches = len(cases).
    pub fn visit_match(&mut self, cx: &mut WalkCx, node: GNode) {
        self.inc_all_stmts(1);
        let md = cx.eng.md(node.m);
        let n_cases = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Match { cases, .. } => cases.len() as i64,
            _ => 0,
        };
        self.inc_branch(cx, node, n_cases);
    }
}

/// _count_boolean_expressions (design_analysis.py:222-233): recursive over the
/// BoolOp's children; each non-BoolOp child counts 1, BoolOp children recurse.
fn count_boolean_expressions(cx: &WalkCx, boolop: GNode) -> i64 {
    let md = cx.eng.md(boolop.m);
    let values = match &md.tree.nodes[boolop.n.idx()].kind {
        NodeKind::BoolOp { values, .. } => values.clone(),
        _ => return 0,
    };
    let mut nb = 0i64;
    for v in values {
        let child = GNode { m: boolop.m, n: v };
        if matches!(md.tree.nodes[v.idx()].kind, NodeKind::BoolOp { .. }) {
            nb += count_boolean_expressions(cx, child);
        } else {
            nb += 1;
        }
    }
    nb
}
