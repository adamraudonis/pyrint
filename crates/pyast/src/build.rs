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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FuncKind {
    Function,
    Method,
    ClassMethod,
    StaticMethod,
}

struct ScopeCtx {
    scope: NodeId,
    /// comprehension scopes are not frames (astroid NodeNG.frame() walks
    /// past GeneratorExp/ListComp/SetComp/DictComp)
    is_comprehension: bool,
}

pub struct Builder<'a> {
    src: &'a SourceFile,
    nodes: Vec<Node>,
    interner: Interner,
    locals: rustc_hash::FxHashMap<NodeId, indexmap::IndexMap<Sym, Vec<NodeId>>>,
    /// `global` name sets per FUNCTION frame only (astroid rebuilder
    /// _global_names: pushed in _visit_functiondef / visit_lambda, so class
    /// bodies nested in a function share the function's set).
    global_stack: Vec<rustc_hash::FxHashSet<Sym>>,
    /// ImportFrom nodes for delayed locals insertion, with the `global`
    /// names captured at visit time (astroid rebuilder.py:1102).
    delayed_import_from: Vec<(NodeId, rustc_hash::FxHashSet<Sym>)>,
    /// token start offsets of `def` / `class` keywords, sorted
    def_class_tokens: Vec<(TextSize, bool)>, // (offset, is_def)
    /// token start offsets of `async` keywords, sorted
    async_tokens: Vec<(TextSize, bool)>,
    /// all tokens triaged for paren matching: (start, end, kind)
    /// kind: 0 = other, 1 = '(', 2 = ')', 3 = trivia (comments/non-logical newlines)
    all_tokens: Vec<(u32, u32, u8)>,
    scope_stack: Vec<ScopeCtx>,
    /// stack of Arguments nodes being built (to spot walrus targets whose
    /// NamedExpr is a DIRECT child of Arguments, see NamedExpr.frame())
    arguments_stack: Vec<NodeId>,
    /// AssignAttr nodes for astroid builder.delayed_assattr
    delayed_assattr: Vec<NodeId>,
    /// every NamedExpr node built (used to spot walruses in dataclass
    /// attribute defaults, which break astroid's generated __init__)
    walrus_ids: Vec<NodeId>,
}

impl<'a> Builder<'a> {
    pub fn build(
        src: &'a SourceFile,
        parsed: &ast::ModModule,
        tokens: &[(TextSize, bool)],
        async_tokens: &[(TextSize, bool)],
        all_tokens: &[(u32, u32, u8)],
        opts: &BuildOptions,
    ) -> Tree {
        let mut b = Builder {
            src,
            nodes: Vec::with_capacity(1024),
            interner: Interner::default(),
            locals: rustc_hash::FxHashMap::default(),
            global_stack: Vec::new(),
            delayed_import_from: Vec::new(),
            def_class_tokens: tokens.to_vec(),
            async_tokens: async_tokens.to_vec(),
            all_tokens: all_tokens.to_vec(),
            scope_stack: Vec::new(),
            arguments_stack: Vec::new(),
            delayed_assattr: Vec::new(),
            walrus_ids: Vec::new(),
        };
        // Module node id 0
        let module_id = b.push_placeholder();
        b.locals.entry(module_id).or_default();
        b.scope_stack.push(ScopeCtx { scope: module_id, is_comprehension: false });

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
        for (if_id, globals) in std::mem::take(&mut b.delayed_import_from) {
            b.delayed_import_from_locals(if_id, &globals);
        }

        // delayed AssignAttr (astroid builder.delayed_assattr): visible
        // effect is Class.attr = ... adding `attr` to the class's locals
        b.process_delayed_assattr();

        // brain transforms (astroid applies them after delayed locals,
        // see builder.py _post_build -> visit_transforms)
        b.apply_brains();

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
    /// function frame, assign to module locals instead.
    fn save_assignment(&mut self, name: Sym, node: NodeId) {
        let is_global = self
            .global_stack
            .last()
            .is_some_and(|s| s.contains(&name));
        let scope = if is_global {
            NodeId::MODULE
        } else {
            self.cur_scope()
        };
        self.set_local(scope, name, node);
    }

    /// astroid NamedExpr.set_local: walrus targets land in the nearest
    /// FRAME (Module/FunctionDef/Lambda/ClassDef; comprehension scopes are
    /// skipped). A NamedExpr that is a DIRECT child of Arguments (a default
    /// or annotation) escapes the function too (Arguments.parent.parent
    /// .frame()). `global` declarations still win (AssignName goes through
    /// _save_assignment first).
    fn walrus_assignment(&mut self, name: Sym, node: NodeId, namedexpr_parent: NodeId) {
        if self
            .global_stack
            .last()
            .is_some_and(|s| s.contains(&name))
        {
            self.set_local(NodeId::MODULE, name, node);
            return;
        }
        let mut skip_one_frame = self.arguments_stack.last() == Some(&namedexpr_parent);
        for ctx in self.scope_stack.iter().rev() {
            if ctx.is_comprehension {
                continue;
            }
            if skip_one_frame && ctx.scope != NodeId::MODULE {
                skip_one_frame = false;
                continue;
            }
            let scope = ctx.scope;
            self.set_local(scope, name, node);
            return;
        }
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
                    // anchor at the `async` token directly before (CPython
                    // gives AsyncFunctionDef the `async` keyword position)
                    if let Some(&(aoff, _)) = self.async_tokens_before(off) {
                        off = aoff;
                    }
                }
                TextRange::new(off, body_range.end())
            }
            None => body_range,
        }
    }

    /// Extend a range to the enclosing parens, skipping trivia tokens.
    fn extend_parens(&self, range: TextRange) -> TextRange {
        let start = range.start().to_u32();
        let end = range.end().to_u32();
        // backward: last non-trivia token ending at or before start
        let mut i = self.all_tokens.partition_point(|t| t.1 <= start);
        let mut new_start = range.start();
        while i > 0 {
            i -= 1;
            match self.all_tokens[i].2 {
                3 => continue,
                1 => {
                    new_start = TextSize::from(self.all_tokens[i].0);
                    break;
                }
                _ => break,
            }
        }
        if new_start == range.start() {
            return range; // no opening paren found; keep as-is
        }
        // forward: first non-trivia token starting at or after end
        let mut j = self.all_tokens.partition_point(|t| t.0 < end);
        let mut new_end = range.end();
        while j < self.all_tokens.len() {
            match self.all_tokens[j].2 {
                3 => j += 1,
                2 => {
                    new_end = TextSize::from(self.all_tokens[j].1);
                    break;
                }
                _ => break,
            }
        }
        if new_end == range.end() {
            return range;
        }
        TextRange::new(new_start, new_end)
    }

    /// Match CPython's tokenizer type-comment prefix `"# type: "` against
    /// `text` (which must start at a `#`). Each space in the prefix matches
    /// any run of spaces/tabs. Returns the payload (text after the prefix),
    /// or None. `# type: ignore...` (per CPython's TYPE_IGNORE rule) yields
    /// None as it never attaches to statements.
    fn type_comment_payload(text: &str) -> Option<&str> {
        let mut rest = text.strip_prefix('#')?;
        rest = rest.trim_start_matches([' ', '\t']);
        rest = rest.strip_prefix("type:")?;
        rest = rest.trim_start_matches([' ', '\t']);
        // TYPE_IGNORE: "ignore" followed by EOL or a non-alphanumeric char
        if let Some(after) = rest.strip_prefix("ignore") {
            match after.bytes().next() {
                None => return None,
                Some(c) if !c.is_ascii_alphanumeric() => return None,
                _ => {}
            }
        }
        Some(rest)
    }

    /// If the source line containing `range.end()` continues with only
    /// whitespace and then a `# type:` comment, extend the range to the end
    /// of that line (CPython's TYPE_COMMENT token is part of the Assign).
    fn extend_assign_type_comment(&self, range: TextRange) -> TextRange {
        let end = range.end().to_u32() as usize;
        let text = &self.src.text;
        let line_end = text[end..]
            .find('\n')
            .map(|i| end + i)
            .unwrap_or(text.len());
        let mut tail = &text[end..line_end];
        if tail.ends_with('\r') {
            tail = &tail[..tail.len() - 1];
        }
        let trimmed = tail.trim_start_matches([' ', '\t']);
        if !trimmed.starts_with('#') {
            return range;
        }
        if Self::type_comment_payload(trimmed).is_none() {
            return range;
        }
        let new_end = end + tail.len();
        TextRange::new(range.start(), TextSize::from(new_end as u32))
    }

    /// Whether a *valid* per-argument type comment (`a,  # type: int`)
    /// attaches to the parameter ending at `param_end`, looking up to
    /// `limit` (start of the next parameter, or end of the parameter list).
    /// "Valid" mirrors astroid check_type_comment: the payload must parse as
    /// a module whose first statement is an expression.
    fn arg_has_type_comment(&self, param_end: TextSize, limit: TextSize) -> bool {
        let lo = param_end.to_u32();
        let hi = limit.to_u32();
        let i = self.all_tokens.partition_point(|t| t.0 < lo);
        for t in &self.all_tokens[i..] {
            if t.0 >= hi {
                break;
            }
            let s = &self.src.text[t.0 as usize..t.1 as usize];
            if !s.starts_with('#') {
                continue;
            }
            if let Some(payload) = Self::type_comment_payload(s) {
                // astroid parses the payload; only Expr statements count
                let parsed = ruff_python_parser::parse_module(payload);
                if let Ok(m) = parsed {
                    if let Some(ruff_python_ast::Stmt::Expr(_)) = m.syntax().body.first() {
                        return true;
                    }
                }
                return false;
            }
        }
        false
    }

    /// If any literal part contains a `\\u`/`\\U` escape that decodes to a
    /// lone surrogate, decode the WHOLE string ourselves into raw code
    /// points (CPython semantics). Returns None when no surrogates appear
    /// (or when we bail on \\N{...} escapes).
    fn decode_str_with_surrogates(&self, s: &ast::ExprStringLiteral) -> Option<Vec<u32>> {
        let mut might = false;
        for part in s.value.iter() {
            if part.flags.prefix().is_raw() {
                continue;
            }
            let src = &self.src.text
                [part.range.start().to_u32() as usize..part.range.end().to_u32() as usize];
            if src.contains("\\u") || src.contains("\\U") {
                might = true;
            }
        }
        if !might {
            return None;
        }
        let mut points: Vec<u32> = Vec::new();
        let mut found_surrogate = false;
        for part in s.value.iter() {
            if part.flags.prefix().is_raw() {
                // raw parts cannot produce escapes; copy decoded value
                points.extend(part.value.chars().map(|c| c as u32));
                continue;
            }
            use ruff_python_ast::StringFlags as _;
            let inner_start = part.range.start().to_u32() + part.flags.opener_len().to_u32();
            let inner_end = part.range.end().to_u32() - part.flags.closer_len().to_u32();
            let inner = &self.src.text[inner_start as usize..inner_end as usize];
            if !Self::unescape_py(inner, &mut points, &mut found_surrogate) {
                return None; // \\N{...} or malformed: trust ruff's value
            }
        }
        if found_surrogate { Some(points) } else { None }
    }

    /// CPython non-raw str escape decoding into raw code points. Returns
    /// false to bail (\\N escapes need the Unicode name DB).
    fn unescape_py(inner: &str, out: &mut Vec<u32>, found_surrogate: &mut bool) -> bool {
        let b: Vec<char> = inner.chars().collect();
        let mut i = 0usize;
        let hexval = |cs: &[char]| -> Option<u32> {
            let s: String = cs.iter().collect();
            u32::from_str_radix(&s, 16).ok()
        };
        while i < b.len() {
            let c = b[i];
            if c != '\\' {
                out.push(c as u32);
                i += 1;
                continue;
            }
            if i + 1 >= b.len() {
                out.push('\\' as u32);
                break;
            }
            let e = b[i + 1];
            i += 2;
            match e {
                '\n' => {}
                '\r' => {
                    // line continuation over \r\n
                    if i < b.len() && b[i] == '\n' {
                        i += 1;
                    }
                }
                '\\' | '\'' | '"' => out.push(e as u32),
                'a' => out.push(7),
                'b' => out.push(8),
                'f' => out.push(12),
                'n' => out.push(10),
                'r' => out.push(13),
                't' => out.push(9),
                'v' => out.push(11),
                '0'..='7' => {
                    let mut v = e as u32 - '0' as u32;
                    let mut n = 1;
                    while n < 3 && i < b.len() && ('0'..='7').contains(&b[i]) {
                        v = v * 8 + (b[i] as u32 - '0' as u32);
                        i += 1;
                        n += 1;
                    }
                    out.push(v);
                }
                'x' => {
                    if i + 2 > b.len() {
                        return false;
                    }
                    match hexval(&b[i..i + 2]) {
                        Some(v) => out.push(v),
                        None => return false,
                    }
                    i += 2;
                }
                'u' => {
                    if i + 4 > b.len() {
                        return false;
                    }
                    match hexval(&b[i..i + 4]) {
                        Some(v) => {
                            if (0xD800..=0xDFFF).contains(&v) {
                                *found_surrogate = true;
                            }
                            out.push(v);
                        }
                        None => return false,
                    }
                    i += 4;
                }
                'U' => {
                    if i + 8 > b.len() {
                        return false;
                    }
                    match hexval(&b[i..i + 8]) {
                        Some(v) => {
                            if (0xD800..=0xDFFF).contains(&v) {
                                *found_surrogate = true;
                            }
                            out.push(v);
                        }
                        None => return false,
                    }
                    i += 8;
                }
                'N' => return false,
                other => {
                    // unknown escape: CPython keeps backslash + char
                    out.push('\\' as u32);
                    out.push(other as u32);
                }
            }
        }
        true
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
                    NodeKind::Const(ConstValue::Str(_) | ConstValue::StrSurrogate(_))
                ) {
                    body.remove(0);
                    return (Some(v), body);
                }
            }
        }
        (None, body)
    }

    fn delayed_import_from_locals(
        &mut self,
        if_id: NodeId,
        globals: &rustc_hash::FxHashSet<Sym>,
    ) {
        // astroid builder.add_from_names_to_locals: names go to the
        // ImportFrom's parent scope, except names declared `global` in the
        // surrounding function which go to the module.
        let parent = self.nodes[if_id.idx()].parent;
        let scope = self.frame_of(parent);
        if let NodeKind::ImportFrom { names, .. } = &self.nodes[if_id.idx()].kind {
            let names: Vec<(Sym, Option<Sym>)> = names.clone();
            let modname = match &self.nodes[if_id.idx()].kind {
                NodeKind::ImportFrom { modname, level, .. } => {
                    if level.is_some() {
                        None // relative: unresolvable with modname "mod"
                    } else {
                        Some(self.interner.get(*modname).to_string())
                    }
                }
                _ => None,
            };
            for (name, asname) in names {
                let n = self.interner.get(name).to_string();
                if n == "*" {
                    // astroid resolves the module and adds public_names();
                    // only stdlib modules resolve in the pinned harness
                    // environment (see harness/gen_stdlib_wildcard.py).
                    if let Some(m) = &modname {
                        if let Some(pubnames) = crate::stdlib_wildcard::wildcard_names(m) {
                            for pn in pubnames {
                                let local = self.sym(pn);
                                let target = if globals.contains(&local) {
                                    NodeId::MODULE
                                } else {
                                    scope
                                };
                                self.set_local(target, local, if_id);
                            }
                        }
                    }
                    continue;
                }
                let local = asname.unwrap_or(name);
                let target = if globals.contains(&local) {
                    NodeId::MODULE
                } else {
                    scope
                };
                self.set_local(target, local, if_id);
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

    // ---------- brain transforms (astroid brain/*.py, dump-visible only) ----------

    /// astroid as_string() for plain dotted names (Name / Attribute chains).
    fn dotted_name(&self, id: NodeId) -> Option<String> {
        match &self.nodes[id.idx()].kind {
            NodeKind::Name { name } => Some(self.interner.get(*name).to_string()),
            NodeKind::Attribute { expr, attrname, .. } => {
                let base = self.dotted_name(*expr)?;
                Some(format!("{}.{}", base, self.interner.get(*attrname)))
            }
            _ => None,
        }
    }

    /// All bindings of `name` walking the scope chain starting at `scope`.
    fn lookup_bindings(&self, mut scope: NodeId, name: Sym) -> Vec<NodeId> {
        loop {
            if let Some(map) = self.locals.get(&scope) {
                if let Some(v) = map.get(&name) {
                    if !v.is_empty() {
                        return v.clone();
                    }
                }
            }
            if scope == NodeId::MODULE {
                return Vec::new();
            }
            let p = self.nodes[scope.idx()].parent;
            scope = self.frame_of(p);
        }
    }

    fn statement_of(&self, mut id: NodeId) -> NodeId {
        use NodeKind::*;
        loop {
            if matches!(
                self.nodes[id.idx()].kind,
                Module(_) | FunctionDef(_) | AsyncFunctionDef(_) | ClassDef(_)
                    | Return { .. } | Delete { .. } | Assign { .. } | AugAssign { .. }
                    | AnnAssign { .. } | TypeAlias { .. } | For(_) | AsyncFor(_)
                    | While { .. } | If { .. } | With(_) | AsyncWith(_) | Match { .. }
                    | Raise { .. } | Try(_) | TryStar(_) | Assert { .. } | Import { .. }
                    | ImportFrom { .. } | Global { .. } | Nonlocal { .. } | Expr { .. }
                    | Pass | Break | Continue | ExceptHandler { .. }
            ) {
                return id;
            }
            let p = self.nodes[id.idx()].parent;
            if p == id {
                return id;
            }
            id = p;
        }
    }

    /// brain_namedtuple_enum._is_enum_subclass, approximated without full
    /// inference: a base resolves to stdlib enum.{Enum,IntEnum,...} via this
    /// module's imports, or to a local ClassDef that is itself one.
    fn is_enum_subclass(
        &self,
        class_id: NodeId,
        memo: &mut rustc_hash::FxHashMap<NodeId, bool>,
    ) -> bool {
        const ENUM_NAMES: &[&str] = &["Enum", "IntEnum", "StrEnum", "Flag", "IntFlag", "ReprEnum"];
        if let Some(&v) = memo.get(&class_id) {
            return v;
        }
        memo.insert(class_id, false); // cycle guard
        let bases: Vec<NodeId> = match &self.nodes[class_id.idx()].kind {
            NodeKind::ClassDef(d) => d.bases.clone(),
            _ => return false,
        };
        let start_scope = self.frame_of(self.nodes[class_id.idx()].parent);
        let mut result = false;
        'outer: for base in bases {
            match &self.nodes[base.idx()].kind {
                NodeKind::Name { name } => {
                    let n = *name;
                    for b in self.lookup_bindings(start_scope, n) {
                        match &self.nodes[b.idx()].kind {
                            NodeKind::ImportFrom { modname, names, .. } => {
                                if self.interner.get(*modname) == "enum" {
                                    for (orig, asname) in names {
                                        let local = asname.unwrap_or(*orig);
                                        if local == n
                                            && ENUM_NAMES.contains(&self.interner.get(*orig))
                                        {
                                            result = true;
                                            break 'outer;
                                        }
                                    }
                                }
                            }
                            NodeKind::ClassDef(_) => {
                                if self.is_enum_subclass(b, memo) {
                                    result = true;
                                    break 'outer;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                NodeKind::Attribute { expr, attrname, .. } => {
                    if ENUM_NAMES.contains(&self.interner.get(*attrname)) {
                        if let NodeKind::Name { name: m } = &self.nodes[expr.idx()].kind {
                            let m = *m;
                            for b in self.lookup_bindings(start_scope, m) {
                                if let NodeKind::Import { names } = &self.nodes[b.idx()].kind {
                                    for (full, asname) in names {
                                        let local = match asname {
                                            Some(a) => *a,
                                            None => {
                                                let f = self.interner.get(*full);
                                                if f == "enum" { *full } else { continue }
                                            }
                                        };
                                        if local == m && self.interner.get(*full) == "enum" {
                                            result = true;
                                            break 'outer;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        memo.insert(class_id, result);
        result
    }

    /// brain_dataclasses._looks_like_dataclass_decorator, approximated for a
    /// venv where third-party modules are NOT importable (so astroid's
    /// inference falls back to literal name matching for them).
    fn looks_like_dataclass_decorator(&self, dec: NodeId, start_scope: NodeId) -> bool {
        const DATACLASS_MODULES: &[&str] =
            &["dataclasses", "marshmallow_dataclass", "pydantic.dataclasses"];
        let func = match &self.nodes[dec.idx()].kind {
            NodeKind::Call { func, .. } => *func,
            _ => dec,
        };
        match &self.nodes[func.idx()].kind {
            NodeKind::Attribute { attrname, .. } => self.interner.get(*attrname) == "dataclass",
            NodeKind::Name { name } => {
                let n = *name;
                let literal = self.interner.get(n) == "dataclass";
                let bindings = self.lookup_bindings(start_scope, n);
                if bindings.is_empty() {
                    // unbound name: inference fails -> fallback name match
                    return literal;
                }
                for b in &bindings {
                    match &self.nodes[b.idx()].kind {
                        NodeKind::ImportFrom { modname, names, .. } => {
                            let m = self.interner.get(*modname).to_string();
                            for (orig, asname) in names {
                                let local = asname.unwrap_or(*orig);
                                if local != n {
                                    continue;
                                }
                                if DATACLASS_MODULES.contains(&m.as_str())
                                    && self.interner.get(*orig) == "dataclass"
                                {
                                    return true;
                                }
                                // unresolvable (third-party) module: astroid
                                // inference fails -> literal name fallback
                                if m != "dataclasses" && literal {
                                    return true;
                                }
                            }
                        }
                        NodeKind::Import { .. } => {
                            if literal {
                                return true;
                            }
                        }
                        _ => {} // local def/class/assign: infers non-dataclass
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// astroid builder.delayed_assattr: for every `x.attr = ...`, infer x;
    /// visible effects (in node.locals, which the dump shows):
    /// - `LocalClass.attr = ...`            -> class locals
    /// - `cls.attr = ...` in a classmethod  -> class locals
    /// - first-arg `.attr = ...` in any method of a METACLASS -> class locals
    /// - `self.__class__.attr = ...` in a method -> class locals
    /// - `self.X.attr = ...` where self.X was assigned a local class -> that
    ///   class's locals
    /// - `f.attr = ...` where f is a lambda  -> the Lambda's locals
    /// Instances and named functions only get invisible instance_attrs.
    fn process_delayed_assattr(&mut self) {
        let delayed = std::mem::take(&mut self.delayed_assattr);
        let mut meta_memo: rustc_hash::FxHashMap<NodeId, bool> = Default::default();

        // pre-pass: map (class, attr) -> classes assigned via `self.attr = LocalClass`
        let mut self_attr_map: rustc_hash::FxHashMap<(NodeId, Sym), Vec<NodeId>> =
            Default::default();
        for &id in &delayed {
            let NodeKind::AssignAttr { expr, attrname } = &self.nodes[id.idx()].kind else {
                continue;
            };
            let (expr, attrname) = (*expr, *attrname);
            if !matches!(self.nodes[expr.idx()].kind, NodeKind::Name { .. }) {
                continue;
            }
            let Some((cls, FuncKind::Method)) = self.first_param_class(expr) else {
                continue;
            };
            let assign = self.nodes[id.idx()].parent;
            let NodeKind::Assign { value, .. } = &self.nodes[assign.idx()].kind else {
                continue;
            };
            let NodeKind::Name { name: vname } = &self.nodes[value.idx()].kind else {
                continue;
            };
            let vscope = self.frame_of(self.nodes[value.idx()].parent);
            for b in self.lookup_bindings(vscope, *vname) {
                if matches!(self.nodes[b.idx()].kind, NodeKind::ClassDef(_)) {
                    self_attr_map.entry((cls, attrname)).or_default().push(b);
                }
            }
        }

        for id in delayed {
            let (expr, attrname) = match &self.nodes[id.idx()].kind {
                NodeKind::AssignAttr { expr, attrname } => (*expr, *attrname),
                _ => continue,
            };
            let mut targets: Vec<NodeId> = Vec::new();
            match &self.nodes[expr.idx()].kind {
                NodeKind::Name { name } => {
                    let name = *name;
                    let scope = self.frame_of(self.nodes[expr.idx()].parent);
                    for b in self.lookup_bindings(scope, name) {
                        match &self.nodes[b.idx()].kind {
                            NodeKind::ClassDef(_) => targets.push(b),
                            NodeKind::AssignName { .. } => {
                                // (a) first parameter of a classmethod / a
                                // metaclass method infers to the class
                                if let Some((cls, kind)) = self.param_class_of_binding(b) {
                                    match kind {
                                        FuncKind::ClassMethod => targets.push(cls),
                                        FuncKind::Method => {
                                            if self.is_metaclass_class(cls, &mut meta_memo) {
                                                targets.push(cls);
                                            }
                                        }
                                        _ => {}
                                    }
                                    continue;
                                }
                                // (b) name assigned a lambda: astroid routes
                                // the attr into the Lambda's locals
                                let stmt = self.statement_of(b);
                                if let NodeKind::Assign { value, .. } =
                                    &self.nodes[stmt.idx()].kind
                                {
                                    if matches!(
                                        self.nodes[value.idx()].kind,
                                        NodeKind::Lambda(_)
                                    ) {
                                        targets.push(*value);
                                    }
                                }
                                // (c) `for k, v in D.items(): v.attr = ...`
                                // where D is a dict of calls to local
                                // functions returning local classes (astroid
                                // infers through the whole chain)
                                targets.extend(self.dict_items_value_classes(b, stmt));
                            }
                            _ => {}
                        }
                    }
                }
                NodeKind::Attribute { expr: inner, attrname: a, .. } => {
                    let (inner, a) = (*inner, *a);
                    if matches!(self.nodes[inner.idx()].kind, NodeKind::Name { .. }) {
                        if let Some((cls, FuncKind::Method)) = self.first_param_class(inner) {
                            if self.interner.get(a) == "__class__" {
                                // self.__class__ infers to the class
                                targets.push(cls);
                            } else if let Some(classes) = self_attr_map.get(&(cls, a)) {
                                targets.extend(classes.iter().copied());
                            }
                        }
                    }
                }
                _ => {}
            }
            for cls in targets {
                let values = self
                    .locals
                    .entry(cls)
                    .or_default()
                    .entry(attrname)
                    .or_default();
                if !values.contains(&id) {
                    values.push(id);
                }
            }
        }
    }

    /// If `name_node` (a Name use) resolves to the first parameter of a
    /// method defined directly in a class, return (class, function kind).
    fn first_param_class(&self, name_node: NodeId) -> Option<(NodeId, FuncKind)> {
        let NodeKind::Name { name } = &self.nodes[name_node.idx()].kind else {
            return None;
        };
        let scope = self.frame_of(self.nodes[name_node.idx()].parent);
        for b in self.lookup_bindings(scope, *name) {
            if matches!(self.nodes[b.idx()].kind, NodeKind::AssignName { .. }) {
                if let Some(res) = self.param_class_of_binding(b) {
                    return Some(res);
                }
            }
        }
        None
    }

    /// If binding `b` is the first parameter of a function whose frame is a
    /// class, return (class, function kind).
    fn param_class_of_binding(&self, b: NodeId) -> Option<(NodeId, FuncKind)> {
        let args_id = self.nodes[b.idx()].parent;
        let NodeKind::Arguments(a) = &self.nodes[args_id.idx()].kind else {
            return None;
        };
        let first = a.posonlyargs.first().or(a.args.first()).copied();
        if first != Some(b) {
            return None;
        }
        let func = self.nodes[args_id.idx()].parent;
        if !matches!(
            self.nodes[func.idx()].kind,
            NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)
        ) {
            return None;
        }
        let cls = self.frame_of(self.nodes[func.idx()].parent);
        if !matches!(self.nodes[cls.idx()].kind, NodeKind::ClassDef(_)) {
            return None;
        }
        Some((cls, self.func_kind(func, cls)))
    }

    /// Narrow replica of astroid's inference for the pattern
    /// `for key, v in SOME_DICT.items(): v.attr = ...` where SOME_DICT is a
    /// local dict literal whose values are local classes or calls to local
    /// functions that `return <LocalClass>` (django/template/smartif.py).
    fn dict_items_value_classes(&self, binding: NodeId, stmt: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let NodeKind::For(fd) = &self.nodes[stmt.idx()].kind else {
            return out;
        };
        // binding must be the SECOND element of the unpacking tuple
        let NodeKind::Tuple { elts, .. } = &self.nodes[fd.target.idx()].kind else {
            return out;
        };
        if elts.len() != 2 || elts[1] != binding {
            return out;
        }
        let NodeKind::Call { func, args, keywords } = &self.nodes[fd.iter.idx()].kind else {
            return out;
        };
        if !args.is_empty() || !keywords.is_empty() {
            return out;
        }
        let NodeKind::Attribute { expr: dexpr, attrname, .. } = &self.nodes[func.idx()].kind
        else {
            return out;
        };
        if self.interner.get(*attrname) != "items" {
            return out;
        }
        let NodeKind::Name { name: dname } = &self.nodes[dexpr.idx()].kind else {
            return out;
        };
        let dscope = self.frame_of(self.nodes[dexpr.idx()].parent);
        for db in self.lookup_bindings(dscope, *dname) {
            if !matches!(self.nodes[db.idx()].kind, NodeKind::AssignName { .. }) {
                continue;
            }
            let dstmt = self.statement_of(db);
            let NodeKind::Assign { value, .. } = &self.nodes[dstmt.idx()].kind else {
                continue;
            };
            let NodeKind::Dict { items } = &self.nodes[value.idx()].kind else {
                continue;
            };
            for (_, v) in items {
                match &self.nodes[v.idx()].kind {
                    NodeKind::Name { name } => {
                        let vscope = self.frame_of(self.nodes[v.idx()].parent);
                        for vb in self.lookup_bindings(vscope, *name) {
                            if matches!(self.nodes[vb.idx()].kind, NodeKind::ClassDef(_)) {
                                out.push(vb);
                            }
                        }
                    }
                    NodeKind::Call { func: cf, .. } => {
                        let NodeKind::Name { name: fname } = &self.nodes[cf.idx()].kind else {
                            continue;
                        };
                        let fscope = self.frame_of(self.nodes[cf.idx()].parent);
                        for fb in self.lookup_bindings(fscope, *fname) {
                            let NodeKind::FunctionDef(fdata) = &self.nodes[fb.idx()].kind
                            else {
                                continue;
                            };
                            for &bstmt in &fdata.body {
                                let NodeKind::Return { value: Some(rv) } =
                                    &self.nodes[bstmt.idx()].kind
                                else {
                                    continue;
                                };
                                let NodeKind::Name { name: rname } =
                                    &self.nodes[rv.idx()].kind
                                else {
                                    continue;
                                };
                                for rb in self.lookup_bindings(fb, *rname) {
                                    if matches!(
                                        self.nodes[rb.idx()].kind,
                                        NodeKind::ClassDef(_)
                                    ) {
                                        out.push(rb);
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// astroid FunctionDef.type (the parts we can compute without full
    /// inference): extra_decorators (`m = classmethod(m)` in the class
    /// body), implicit classmethods, then literal decorators.
    fn func_kind(&self, func: NodeId, class_id: NodeId) -> FuncKind {
        let (name, decorators) = match &self.nodes[func.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => (d.name, d.decorators),
            _ => return FuncKind::Function,
        };
        if let Some(k) = self.extra_decorator_kind(name, class_id) {
            return k;
        }
        let n = self.interner.get(name);
        if matches!(n, "__new__" | "__init_subclass__" | "__class_getitem__") {
            return FuncKind::ClassMethod;
        }
        if let Some(dec_id) = decorators {
            if let NodeKind::Decorators { nodes } = &self.nodes[dec_id.idx()].kind {
                for &d in nodes {
                    match self.dotted_name(d).as_deref() {
                        Some("classmethod") | Some("builtins.classmethod") => {
                            return FuncKind::ClassMethod
                        }
                        Some("staticmethod") | Some("builtins.staticmethod") => {
                            return FuncKind::StaticMethod
                        }
                        _ => {}
                    }
                }
            }
        }
        FuncKind::Method
    }

    /// astroid FunctionDef.extra_decorators: `name = classmethod(name)`
    /// style rebinding in the class body (any nesting within the same
    /// frame), provided the first binding of `name` is the function.
    fn extra_decorator_kind(&self, fname: Sym, class_id: NodeId) -> Option<FuncKind> {
        let first_is_func = self
            .locals
            .get(&class_id)
            .and_then(|m| m.get(&fname))
            .and_then(|v| v.first())
            .is_some_and(|&n| {
                matches!(
                    self.nodes[n.idx()].kind,
                    NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)
                )
            });
        if !first_is_func {
            return None;
        }
        let body = match &self.nodes[class_id.idx()].kind {
            NodeKind::ClassDef(d) => d.body.clone(),
            _ => return None,
        };
        let mut stack = body;
        while let Some(stmt) = stack.pop() {
            match &self.nodes[stmt.idx()].kind {
                NodeKind::Assign { targets, value } => {
                    let hits_name = targets.iter().any(|&t| {
                        matches!(&self.nodes[t.idx()].kind,
                            NodeKind::AssignName { name } if *name == fname)
                    });
                    if !hits_name {
                        continue;
                    }
                    if let NodeKind::Call { func, .. } = &self.nodes[value.idx()].kind {
                        if let NodeKind::Name { name } = &self.nodes[func.idx()].kind {
                            match self.interner.get(*name) {
                                "classmethod" => return Some(FuncKind::ClassMethod),
                                "staticmethod" => return Some(FuncKind::StaticMethod),
                                _ => {}
                            }
                        }
                    }
                }
                NodeKind::If { body, orelse, .. } | NodeKind::While { body, orelse, .. } => {
                    stack.extend(body.iter().chain(orelse));
                }
                NodeKind::For(d) | NodeKind::AsyncFor(d) => {
                    stack.extend(d.body.iter().chain(&d.orelse));
                }
                NodeKind::With(d) | NodeKind::AsyncWith(d) => stack.extend(&d.body),
                NodeKind::Try(d) | NodeKind::TryStar(d) => {
                    stack.extend(
                        d.body
                            .iter()
                            .chain(&d.handlers)
                            .chain(&d.orelse)
                            .chain(&d.finalbody),
                    );
                }
                NodeKind::ExceptHandler { body, .. } => stack.extend(body),
                _ => {}
            }
        }
        None
    }

    /// astroid ClassDef.type == "metaclass": subclass of builtins.type,
    /// resolved through local classes / the `type` builtin.
    fn is_metaclass_class(
        &self,
        class_id: NodeId,
        memo: &mut rustc_hash::FxHashMap<NodeId, bool>,
    ) -> bool {
        if let Some(&v) = memo.get(&class_id) {
            return v;
        }
        memo.insert(class_id, false);
        let bases: Vec<NodeId> = match &self.nodes[class_id.idx()].kind {
            NodeKind::ClassDef(d) => d.bases.clone(),
            _ => return false,
        };
        let start_scope = self.frame_of(self.nodes[class_id.idx()].parent);
        let mut result = false;
        'outer: for base in bases {
            match &self.nodes[base.idx()].kind {
                NodeKind::Name { name } => {
                    let n = *name;
                    let bindings = self.lookup_bindings(start_scope, n);
                    if bindings.is_empty() && self.interner.get(n) == "type" {
                        result = true;
                        break 'outer;
                    }
                    for b in bindings {
                        if matches!(self.nodes[b.idx()].kind, NodeKind::ClassDef(_))
                            && self.is_metaclass_class(b, memo)
                        {
                            result = true;
                            break 'outer;
                        }
                    }
                }
                NodeKind::Attribute { expr, attrname, .. } => {
                    if self.interner.get(*attrname) == "type" {
                        if let NodeKind::Name { name } = &self.nodes[expr.idx()].kind {
                            if self.interner.get(*name) == "builtins" {
                                result = true;
                                break 'outer;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        memo.insert(class_id, result);
        result
    }

    /// Whether any NamedExpr lies in the subtree rooted at `ancestor`.
    fn has_walrus_within(&self, ancestor: NodeId) -> bool {
        'walrus: for &w in &self.walrus_ids {
            let mut cur = w;
            loop {
                if cur == ancestor {
                    return true;
                }
                let p = self.nodes[cur.idx()].parent;
                if p == cur || cur == NodeId::MODULE {
                    continue 'walrus;
                }
                cur = p;
            }
        }
        false
    }

    /// astroid silently drops the synthesized dataclass __init__ when the
    /// generated source fails to parse. The common trigger is a walrus in an
    /// attribute default: as_string() renders NamedExpr without parentheses.
    fn dataclass_init_unparseable(&self, class_id: NodeId) -> bool {
        let body: Vec<NodeId> = match &self.nodes[class_id.idx()].kind {
            NodeKind::ClassDef(d) => d.body.clone(),
            _ => return false,
        };
        for stmt in body {
            if let NodeKind::AnnAssign { target, value: Some(v), .. } = &self.nodes[stmt.idx()].kind
            {
                if matches!(self.nodes[target.idx()].kind, NodeKind::AssignName { .. })
                    && self.has_walrus_within(*v)
                {
                    return true;
                }
            }
        }
        false
    }

    fn apply_brains(&mut self) {
        const ATTRS_NAMES: &[&str] = &[
            "attr.s", "attrs", "attr.attrs", "attr.attributes", "attr.define",
            "attr.mutable", "attr.frozen", "attrs.define", "attrs.mutable", "attrs.frozen",
        ];
        const COMPOSITE_NAMES: &[&str] = &[
            "composite", "st.composite", "strategies.composite", "hypothesis.strategies.composite",
        ];
        let n = self.nodes.len();
        let mut enum_memo: rustc_hash::FxHashMap<NodeId, bool> = Default::default();
        let ignore_sym = self.sym("_ignore_");
        let name_sym = self.sym("name");
        for i in 0..n {
            let id = NodeId(i as u32);
            match &self.nodes[i].kind {
                // brain_hypothesis: remove the `draw` parameter from
                // @st.composite strategies (sync FunctionDef only).
                NodeKind::FunctionDef(d) => {
                    let (decorators, args_id) = (d.decorators, d.args);
                    let Some(dec_id) = decorators else { continue };
                    let dec_nodes = match &self.nodes[dec_id.idx()].kind {
                        NodeKind::Decorators { nodes } => nodes.clone(),
                        _ => continue,
                    };
                    let first_is_draw = match &self.nodes[args_id.idx()].kind {
                        NodeKind::Arguments(a) => a.args.first().is_some_and(|&f| {
                            matches!(&self.nodes[f.idx()].kind,
                                NodeKind::AssignName { name } if self.interner.get(*name) == "draw")
                        }),
                        _ => false,
                    };
                    if !first_is_draw {
                        continue;
                    }
                    let is_composite = dec_nodes.iter().any(|&dn| {
                        self.dotted_name(dn)
                            .is_some_and(|s| COMPOSITE_NAMES.contains(&s.as_str()))
                    });
                    if is_composite {
                        if let NodeKind::Arguments(a) = &mut self.nodes[args_id.idx()].kind {
                            a.args.remove(0);
                            a.annotations.remove(0);
                        }
                    }
                }
                NodeKind::ClassDef(d) => {
                    let decorators = d.decorators;
                    // --- brain_io: any class literally named BufferedReader/
                    // BufferedWriter gets locals["raw"]; TextIOWrapper gets
                    // locals["buffer"] (predicate is the bare name only).
                    let cname = self.interner.get(d.name);
                    if matches!(cname, "BufferedReader" | "BufferedWriter") {
                        let raw = self.sym("raw");
                        self.locals.entry(id).or_default().entry(raw).or_default();
                    } else if cname == "TextIOWrapper" {
                        let buffer = self.sym("buffer");
                        self.locals.entry(id).or_default().entry(buffer).or_default();
                    }
                    // --- brain_attrs ---
                    if let Some(dec_id) = decorators {
                        let dec_nodes = match &self.nodes[dec_id.idx()].kind {
                            NodeKind::Decorators { nodes } => nodes.clone(),
                            _ => Vec::new(),
                        };
                        let is_attrs = dec_nodes.iter().any(|&dn| {
                            let f = match &self.nodes[dn.idx()].kind {
                                NodeKind::Call { func, .. } => *func,
                                _ => dn,
                            };
                            self.dotted_name(f)
                                .is_some_and(|s| ATTRS_NAMES.contains(&s.as_str()))
                        });
                        if is_attrs {
                            let s = self.sym("__attrs_attrs__");
                            self.locals.entry(id).or_default().entry(s).or_default();
                        }
                        // --- brain_dataclasses ---
                        let start_scope = self.frame_of(self.nodes[i].parent);
                        let is_dc = dec_nodes
                            .iter()
                            .any(|&dn| self.looks_like_dataclass_decorator(dn, start_scope));
                        if is_dc {
                            let init_sym = self.sym("__init__");
                            let has_init = self
                                .locals
                                .get(&id)
                                .is_some_and(|m| m.contains_key(&init_sym));
                            // found = LAST Call decorator that looks like
                            // dataclass; init=False on it disables generation
                            let mut init_false = false;
                            let mut found: Option<NodeId> = None;
                            for &dn in &dec_nodes {
                                if matches!(self.nodes[dn.idx()].kind, NodeKind::Call { .. })
                                    && self.looks_like_dataclass_decorator(dn, start_scope)
                                {
                                    found = Some(dn);
                                }
                            }
                            if let Some(f) = found {
                                if let NodeKind::Call { keywords, .. } = &self.nodes[f.idx()].kind {
                                    for &kw in keywords {
                                        if let NodeKind::Keyword { arg: Some(a), value } =
                                            &self.nodes[kw.idx()].kind
                                        {
                                            if self.interner.get(*a) == "init"
                                                && matches!(
                                                    &self.nodes[value.idx()].kind,
                                                    NodeKind::Const(ConstValue::Bool(false))
                                                        | NodeKind::Const(ConstValue::Int(
                                                            IntValue::Small(0)
                                                        ))
                                                )
                                            {
                                                init_false = true;
                                            }
                                        }
                                    }
                                }
                            }
                            if !has_init && !init_false && !self.dataclass_init_unparseable(id) {
                                self.locals
                                    .entry(id)
                                    .or_default()
                                    .entry(init_sym)
                                    .or_default();
                                let factory = self.sym("_HAS_DEFAULT_FACTORY");
                                self.locals
                                    .entry(NodeId::MODULE)
                                    .or_default()
                                    .entry(factory)
                                    .or_default();
                            }
                        }
                    }
                    // --- brain_namedtuple_enum (infer_enum_class) ---
                    if self.is_enum_subclass(id, &mut enum_memo) {
                        let mut target_names: rustc_hash::FxHashSet<Sym> = Default::default();
                        let entries: Vec<(Sym, Vec<NodeId>)> = self
                            .locals
                            .get(&id)
                            .map(|m| m.iter().map(|(k, v)| (*k, v.clone())).collect())
                            .unwrap_or_default();
                        for (key, values) in entries {
                            if key == ignore_sym || values.is_empty() {
                                continue;
                            }
                            if !values.iter().all(|v| {
                                matches!(self.nodes[v.idx()].kind, NodeKind::AssignName { .. })
                            }) {
                                continue;
                            }
                            let stmt = self.statement_of(values[0]);
                            let targets: Vec<NodeId> = match &self.nodes[stmt.idx()].kind {
                                NodeKind::Assign { targets, .. } => {
                                    match &self.nodes[targets[0].idx()].kind {
                                        NodeKind::Tuple { elts, .. } => elts.clone(),
                                        _ => targets.clone(),
                                    }
                                }
                                NodeKind::AnnAssign { target, .. } => vec![*target],
                                _ => continue,
                            };
                            for t in targets {
                                if let NodeKind::AssignName { name } = &self.nodes[t.idx()].kind {
                                    target_names.insert(*name);
                                }
                            }
                        }
                        let v2m = self.sym("_value2member_map_");
                        let members = self.sym("__members__");
                        let map = self.locals.entry(id).or_default();
                        map.entry(v2m).or_default();
                        map.entry(members).or_default();
                        if !target_names.contains(&name_sym) {
                            map.entry(name_sym).or_default();
                        }
                    }
                }
                _ => {}
            }
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
                // CPython (type_comments=True, as astroid parses) extends an
                // Assign's end position through a trailing `# type:` comment.
                let range = self.extend_assign_type_comment(a.range);
                self.finish(id, NodeKind::Assign { targets, value }, parent, range)
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
                let captured = self.global_stack.last().cloned().unwrap_or_default();
                self.delayed_import_from.push((fin, captured));
                fin
            }
            Stmt::Global(g) => {
                let names: Vec<Sym> = g.names.iter().map(|n| self.sym(n.as_str())).collect();
                // astroid: `global` at module level has no effect (rebuilder.py:1255)
                if let Some(set) = self.global_stack.last_mut() {
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

        // astroid evaluation order: decorators, returns, then postinit
        // (args, body, ..., type_params); name set_local happens LAST
        // (rebuilder._visit_functiondef), so body-level `global` assignments
        // land in the parent scope before the function's own name.
        let decorators = self.decorators(&f.decorator_list, id);
        let returns = f.returns.as_ref().map(|r| self.expr(r, id));
        self.locals.entry(id).or_default();
        self.scope_stack.push(ScopeCtx { scope: id, is_comprehension: false });
        self.global_stack.push(rustc_hash::FxHashSet::default());

        let args = self.arguments(&f.parameters, id, true);
        let body_ids = self.stmts(&f.body, id);
        let (doc_node, body) = self.extract_doc(body_ids);
        let type_params = f
            .type_params
            .as_ref()
            .map(|tp| self.type_params(tp, id))
            .unwrap_or_default();
        self.global_stack.pop();
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
        let fin = self.finish(id, kind, parent, range);
        // astroid FunctionDef.fromlineno quirk (scoped_nodes): lineno is the
        // first decorator's line; fromlineno adds each decorator's span.
        // Comments/blank lines between decorators and `def` are NOT counted,
        // so this can differ from the actual `def` line.
        if let Some(dec_id) = decorators {
            if let NodeKind::Decorators { nodes } = &self.nodes[dec_id.idx()].kind {
                let nodes = nodes.clone();
                if let Some(&first) = nodes.first() {
                    let mut lineno = self.nodes[first.idx()].fromlineno;
                    for d in &nodes {
                        let n = &self.nodes[d.idx()];
                        let to = if n.end_lineno != 0 { n.end_lineno } else { n.fromlineno };
                        lineno += to - n.fromlineno + 1;
                    }
                    self.nodes[id.idx()].fromlineno = lineno;
                }
            }
        }
        // name set_local in the parent scope (plain set_local: astroid uses
        // parent.set_local directly, ignoring `global` declarations)
        let scope = self.cur_scope();
        self.set_local(scope, name, fin);
        fin
    }

    fn class_def(&mut self, id: NodeId, c: &ast::StmtClassDef, parent: NodeId) -> NodeId {
        let name = self.sym(c.name.as_str());

        let decorators = self.decorators(&c.decorator_list, id);
        self.locals.entry(id).or_default();
        // implicit class locals (astroid ClassDef.implicit_locals: always all three)
        for implicit in ["__module__", "__qualname__", "__annotations__"] {
            let s = self.sym(implicit);
            self.locals.get_mut(&id).unwrap().entry(s).or_default();
        }
        // class bodies do NOT push a `global` frame (astroid only pushes
        // _global_names in _visit_functiondef), so nested `global` stmts
        // accumulate in the enclosing function's set.
        self.scope_stack.push(ScopeCtx { scope: id, is_comprehension: false });

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
        let body_ids = self.stmts(&c.body, id);
        let (doc_node, body) = self.extract_doc(body_ids);
        // astroid visits type_params last in postinit: their AssignNames are
        // appended to the class locals after the body's names.
        let type_params = c
            .type_params
            .as_ref()
            .map(|tp| self.type_params(tp, id))
            .unwrap_or_default();

        self.scope_stack.pop();

        let range = self.def_class_pos(c.range, false);
        let fin = self.finish(
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
        );
        // name set_local after children (astroid visit_classdef)
        let scope = self.cur_scope();
        self.set_local(scope, name, fin);
        fin
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

    fn arguments(
        &mut self,
        params: &ast::Parameters,
        parent: NodeId,
        allow_type_comments: bool,
    ) -> NodeId {
        let id = self.push_placeholder();
        self.arguments_stack.push(id);
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

        // locals insertion order mirrors astroid visit_arguments: args,
        // kwonlyargs, then posonlyargs (visited later!), then vararg/kwarg.
        let scope = self.cur_scope();
        for &a in args.iter().chain(kwonlyargs.iter()).chain(posonlyargs.iter()) {
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

        // Per-arg type comments (`a,  # type: int`): astroid stores parsed
        // nodes in type_comment_* fields which are last in _astroid_fields,
        // so NodeNG.last_child / Arguments.tolineno sees them (the parsed
        // node always has tolineno 1). Record presence for the LAST arg of
        // each category; that is all tolineno needs.
        let mut tc_last_posonly = false;
        let mut tc_last_arg = false;
        let mut tc_last_kwonly = false;
        if allow_type_comments {
            let list_end = params.range.end();
            let next_after_posonly = params
                .args
                .first()
                .map(|p| p.range.start())
                .or(params.vararg.as_ref().map(|v| v.range.start()))
                .or(params.kwonlyargs.first().map(|p| p.range.start()))
                .or(params.kwarg.as_ref().map(|v| v.range.start()))
                .unwrap_or(list_end);
            let next_after_args = params
                .vararg
                .as_ref()
                .map(|v| v.range.start())
                .or(params.kwonlyargs.first().map(|p| p.range.start()))
                .or(params.kwarg.as_ref().map(|v| v.range.start()))
                .unwrap_or(list_end);
            let next_after_kwonly = params
                .kwarg
                .as_ref()
                .map(|v| v.range.start())
                .unwrap_or(list_end);
            if let Some(p) = params.posonlyargs.last() {
                tc_last_posonly = self.arg_has_type_comment(p.range.end(), next_after_posonly);
            }
            if let Some(p) = params.args.last() {
                tc_last_arg = self.arg_has_type_comment(p.range.end(), next_after_args);
            }
            if let Some(p) = params.kwonlyargs.last() {
                tc_last_kwonly = self.arg_has_type_comment(p.range.end(), next_after_kwonly);
            }
        }

        self.arguments_stack.pop();
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
                tc_last_posonly,
                tc_last_arg,
                tc_last_kwonly,
            })),
            parent,
        )
    }

    fn param_name(&mut self, p: &ast::Parameter, parent: NodeId) -> NodeId {
        let id = self.push_placeholder();
        let name = self.sym(p.name.as_str());
        // ast.arg spans name AND annotation; astroid passes that range to
        // the AssignName (rebuilder.visit_arg -> visit_assignname).
        self.finish(id, NodeKind::AssignName { name }, parent, p.range())
    }

    fn type_params(&mut self, tp: &ast::TypeParams, parent: NodeId) -> Vec<NodeId> {
        tp.type_params
            .iter()
            .map(|p| {
                let id = self.push_placeholder();
                // astroid visit_typevar/paramspec/typevartuple: the AssignName
                // takes the WHOLE type-param node's position (incl. `*`/`**`
                // and any bound) and is _save_assignment'd into the scope.
                match p {
                    ast::TypeParam::TypeVar(t) => {
                        let nid = self.push_placeholder();
                        let name = self.sym(t.name.as_str());
                        self.finish(nid, NodeKind::AssignName { name }, id, t.range);
                        self.save_assignment(name, nid);
                        let bound = t.bound.as_ref().map(|b| self.expr(b, id));
                        self.finish(id, NodeKind::TypeVar { name: nid, bound }, parent, t.range)
                    }
                    ast::TypeParam::ParamSpec(t) => {
                        let nid = self.push_placeholder();
                        let name = self.sym(t.name.as_str());
                        self.finish(nid, NodeKind::AssignName { name }, id, t.range);
                        self.save_assignment(name, nid);
                        self.finish(id, NodeKind::ParamSpec { name: nid }, parent, t.range)
                    }
                    ast::TypeParam::TypeVarTuple(t) => {
                        let nid = self.push_placeholder();
                        let name = self.sym(t.name.as_str());
                        self.finish(nid, NodeKind::AssignName { name }, id, t.range);
                        self.save_assignment(name, nid);
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
        self.scope_stack.push(ScopeCtx { scope: id, is_comprehension: true });
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
                let target = if let Expr::Name(t) = &*n.target {
                    let tid = self.push_placeholder();
                    let name = self.sym(t.id.as_str());
                    let fin = self.finish(tid, NodeKind::AssignName { name }, id, t.range);
                    self.walrus_assignment(name, fin, parent);
                    fin
                } else {
                    self.expr(&n.target, id)
                };
                let value = self.expr(&n.value, id);
                self.walrus_ids.push(id);
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
                self.scope_stack.push(ScopeCtx { scope: id, is_comprehension: false });
                let args = match &l.parameters {
                    // lambdas cannot carry per-arg type comments
                    Some(p) => self.arguments(p, id, false),
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
                                tc_last_posonly: false,
                                tc_last_arg: false,
                                tc_last_kwonly: false,
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
                // CPython gives genexps paren-inclusive ranges; when the
                // genexp is a sole call argument the call's parens are used.
                let range = if c.parenthesized {
                    c.range
                } else {
                    self.extend_parens(c.range)
                };
                self.finish(
                    id,
                    NodeKind::GeneratorExp(Box::new(CompData { elt, generators })),
                    parent,
                    range,
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
                // ruff replaces lone-surrogate escapes (\ud800) with U+FFFD;
                // CPython keeps them as real (unpaired) code points. Re-decode
                // when a part smells like it contains such an escape.
                let cv = match self.decode_str_with_surrogates(s) {
                    Some(points) => ConstValue::StrSurrogate(points.into_boxed_slice()),
                    None => {
                        let v: String = s.value.to_str().to_string();
                        ConstValue::Str(v.into_boxed_str())
                    }
                };
                self.finish(id, NodeKind::Const(cv), parent, s.range)
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
                let is_store = matches!(a.ctx, ast::ExprContext::Store);
                let kind = match a.ctx {
                    ast::ExprContext::Store => NodeKind::AssignAttr { expr: e, attrname },
                    ast::ExprContext::Del => NodeKind::DelAttr { expr: e, attrname },
                    _ => NodeKind::Attribute {
                        expr: e,
                        attrname,
                        ctx: Ctx::Load,
                    },
                };
                let fin = self.finish(id, kind, parent, a.range);
                if is_store {
                    // astroid rebuilder delays AssignAttr handling
                    self.delayed_assattr.push(fin);
                }
                fin
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
        for part in f.value.iter() {
            match part {
                ast::FStringPart::Literal(lit) => {
                    Self::merge_pending(&mut pending, lit.as_str(), lit.range);
                }
                ast::FStringPart::FString(fs) => {
                    self.fstring_elements(&fs.elements, id, &mut values, &mut pending);
                }
            }
        }
        self.flush_pending(id, &mut values, &mut pending);
        self.finish(id, NodeKind::JoinedStr { values }, parent, f.range)
    }

    fn merge_pending(pending: &mut Option<(String, TextRange)>, text: &str, range: TextRange) {
        match pending {
            Some((s, r)) => {
                s.push_str(text);
                *r = TextRange::new(r.start(), range.end());
            }
            None => *pending = Some((text.to_string(), range)),
        }
    }

    fn flush_pending(
        &mut self,
        parent: NodeId,
        values: &mut Vec<NodeId>,
        pending: &mut Option<(String, TextRange)>,
    ) {
        if let Some((s, range)) = pending.take() {
            let cid = self.push_placeholder();
            self.finish(
                cid,
                NodeKind::Const(ConstValue::Str(s.into_boxed_str())),
                parent,
                range,
            );
            values.push(cid);
        }
    }

    /// Build JoinedStr children from interpolated-string elements (used for
    /// both f-string bodies and, recursively, format specs).
    fn fstring_elements(
        &mut self,
        elements: &ast::InterpolatedStringElements,
        parent: NodeId,
        values: &mut Vec<NodeId>,
        pending: &mut Option<(String, TextRange)>,
    ) {
        for elem in elements.iter() {
            match elem {
                ast::InterpolatedStringElement::Literal(lit) => {
                    Self::merge_pending(pending, &lit.value, lit.range);
                }
                ast::InterpolatedStringElement::Interpolation(fv) => {
                    // f-string `=` debug specifier: CPython appends the raw
                    // source `{`..`=` (leading ws + expr text + trailing
                    // incl. `=`) to the preceding Constant, extending its
                    // range to just past the debug text.
                    if let Some(dbg) = &fv.debug_text {
                        let er = fv.expression.range();
                        let expr_src =
                            &self.src.text[er.start().to_u32() as usize..er.end().to_u32() as usize];
                        let text = format!("{}{}{}", dbg.leading(), expr_src, dbg.trailing());
                        let dbg_end = er.end() + TextSize::from(dbg.trailing().len() as u32);
                        match pending {
                            Some((s, r)) => {
                                s.push_str(&text);
                                *r = TextRange::new(r.start(), dbg_end);
                            }
                            None => {
                                let start = fv.range.start() + TextSize::from(1);
                                *pending = Some((text, TextRange::new(start, dbg_end)));
                            }
                        }
                    }
                    self.flush_pending(parent, values, pending);
                    let fid = self.push_placeholder();
                    let value = self.expr(&fv.expression, fid);
                    let conversion = fv.conversion as i32;
                    let format_spec = fv.format_spec.as_ref().map(|spec| {
                        let sid = self.push_placeholder();
                        let mut spec_values: Vec<NodeId> = Vec::new();
                        let mut spending: Option<(String, TextRange)> = None;
                        self.fstring_elements(&spec.elements, sid, &mut spec_values, &mut spending);
                        self.flush_pending(sid, &mut spec_values, &mut spending);
                        // CPython positions the format_spec JoinedStr
                        // starting AT the colon
                        let spec_range = TextRange::new(
                            spec.range.start() - TextSize::from(1),
                            spec.range.end(),
                        );
                        self.finish(
                            sid,
                            NodeKind::JoinedStr { values: spec_values },
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
                        parent,
                        fv.range,
                    );
                    values.push(fid);
                }
            }
        }
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
            let mut from = fixed_source_line(tree, id);
            // astroid Arguments.fromlineno override (node_classes.py:785):
            // max(super().fromlineno, parent.fromlineno or 0)
            if matches!(tree.nodes[id.idx()].kind, NodeKind::Arguments(_)) {
                let parent = tree.nodes[id.idx()].parent;
                from = from.max(tree.nodes[parent.idx()].fromlineno);
            }
            tree.nodes[id.idx()].fromlineno = from;
        }
        let children = tree.children(id);
        for c in &children {
            rec(tree, *c);
        }
        let n = &tree.nodes[id.idx()];
        let tol = match &n.kind {
            // astroid NodeNG.tolineno via last_child() over REVERSED
            // _astroid_fields, where the type_comment_* lists come last and
            // are lists-of-None whenever the matching arg list is non-empty:
            // a trailing None last_child yields tolineno = fromlineno. With
            // actual per-arg type comments the parsed node has tolineno 1.
            NodeKind::Arguments(d) => {
                if !d.posonlyargs.is_empty() {
                    if d.tc_last_posonly { 1 } else { n.fromlineno }
                } else if !d.kwonlyargs.is_empty() {
                    if d.tc_last_kwonly { 1 } else { n.fromlineno }
                } else if !d.args.is_empty() {
                    if d.tc_last_arg { 1 } else { n.fromlineno }
                } else if let Some(ka) = d.kwargannotation {
                    tree.nodes[ka.idx()].tolineno
                } else if let Some(va) = d.varargannotation {
                    tree.nodes[va.idx()].tolineno
                } else {
                    n.fromlineno
                }
            }
            // astroid Module._astroid_fields = ("doc_node", "body"):
            // last_child is body[-1], else the doc node.
            NodeKind::Module(d) => {
                if let Some(&last) = d.body.last() {
                    tree.nodes[last.idx()].tolineno
                } else if let Some(doc) = d.doc_node {
                    let dn = &tree.nodes[doc.idx()];
                    if dn.end_lineno != 0 { dn.end_lineno } else { dn.fromlineno }
                } else {
                    n.fromlineno
                }
            }
            _ => {
                if n.end_lineno != 0 {
                    n.end_lineno
                } else if let Some(last) = children.last() {
                    tree.nodes[last.idx()].tolineno
                } else {
                    n.fromlineno
                }
            }
        };
        tree.nodes[id.idx()].tolineno = tol;
    }
    rec(tree, NodeId::MODULE);
}
