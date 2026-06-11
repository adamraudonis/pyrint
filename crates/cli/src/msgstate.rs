//! Message-state machine: global (package-scope) state, per-module FileState
//! with astroid block_range expansion, is_message_enabled resolution and the
//! process_tokens pragma loop.
//!
//! Ports:
//! - pylint/lint/message_state_handler.py (process_tokens :347-444,
//!   _is_one_message_enabled :279-313, disable/enable/disable_next :189-232)
//! - pylint/utils/file_state.py (set_msg_status :184-205,
//!   _set_state_on_block_lines :56-90, _set_message_state_in_block :92-162)
//! - astroid block_range: node_ng.py:445-453 (default), scoped_nodes.py:303,
//!   1415, 1972 (Module/FunctionDef/ClassDef), node_classes.py:3033 (If),
//!   3885/3986 (Try/TryStar), 4444 (While), _base_nodes.py:244-256
//!   (_elsed_block_range).

use pyast::parse::{TokEvent, TokEventKind};
use pyast::source::SourceFile;
use pyast::tree::{NodeId, NodeKind, Tree};
use pycheckers::msgstore::{self, get_messages_to_set, MsgIdx, ResolveError};
use rustc_hash::FxHashMap;

use crate::pragma::{option_po_search, parse_pragma, PragmaError};

/// Global ("package") message state: msgid index -> explicit state.
/// Absent = enabled (`_msgs_state.get(msgid, True)`).
pub struct GlobalState {
    pub state: FxHashMap<MsgIdx, bool>,
}

impl GlobalState {
    /// Baseline for the target invocation `-E --disable=<list>`: msgs.rs
    /// `enabled` already folds -E category disables, the explicit disable
    /// list and the E0106 py-version gate (PyLinter.initialize).
    pub fn for_target_flags() -> Self {
        let mut state = FxHashMap::default();
        for (i, m) in pycheckers::msgs::MESSAGES.iter().enumerate() {
            if !m.enabled {
                state.insert(i as MsgIdx, false);
            }
        }
        GlobalState { state }
    }

    /// CLI `--disable=item` (callback_actions._DisableAction): package-scope
    /// disable; resolution errors are stashed by the caller for W0012/R0022.
    pub fn cli_disable(&mut self, item: &str) -> Result<(), ResolveError> {
        let defs = get_messages_to_set(item, false)?;
        for idx in defs {
            self.state.insert(idx, false);
        }
        Ok(())
    }

    pub fn enabled(&self, idx: MsgIdx) -> bool {
        *self.state.get(&idx).unwrap_or(&true)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Module,
    Line,
}

/// Per-module pragma state (pylint FileState).
pub struct FileState<'t> {
    tree: &'t Tree,
    /// post-order node list (children before parent, get_children order)
    postorder: Vec<NodeId>,
    /// _module_msgs_state: msgid -> line -> state (block-EXPANDED)
    pub module_state: FxHashMap<MsgIdx, FxHashMap<u32, bool>>,
    /// _raw_module_msgs_state: msgid -> [(line, state)] in insertion order
    /// (dict semantics: same-line overwrite keeps position); msgid keys in
    /// first-insertion order (python dict semantics — iter_spurious order)
    pub raw_state: indexmap::IndexMap<MsgIdx, Vec<(u32, bool)>>,
    /// _suppression_mapping: (msgid, line) -> ORIGINAL pragma line
    /// (file_state.py:170-176; set on disable, popped on enable)
    pub suppression_mapping: FxHashMap<(MsgIdx, u32), u32>,
    /// module.tolineno (_effective_max_line_number)
    pub max_line: u32,
    /// cache: pragma line -> consuming (innermost containing) node
    block_cache: FxHashMap<u32, Option<NodeId>>,
}

impl<'t> FileState<'t> {
    pub fn new(tree: &'t Tree) -> Self {
        let mut postorder = Vec::with_capacity(tree.nodes.len());
        // iterative post-order over get_children
        let mut stack: Vec<(NodeId, bool)> = vec![(NodeId::MODULE, false)];
        let mut kids: Vec<NodeId> = Vec::new();
        while let Some((id, visited)) = stack.pop() {
            if visited {
                postorder.push(id);
                continue;
            }
            stack.push((id, true));
            kids.clear();
            tree.push_children(id, &mut kids);
            for &k in kids.iter().rev() {
                stack.push((k, false));
            }
        }
        let max_line = tree.node(NodeId::MODULE).tolineno;
        FileState {
            tree,
            postorder,
            module_state: FxHashMap::default(),
            raw_state: indexmap::IndexMap::default(),
            suppression_mapping: FxHashMap::default(),
            max_line,
            block_cache: FxHashMap::default(),
        }
    }

    /// FileState.set_msg_status (file_state.py:184-205).
    pub fn set_msg_status(&mut self, idx: MsgIdx, line: u32, status: bool, scope: Scope) {
        debug_assert!(line > 0);
        if scope != Scope::Line {
            self.set_state_on_block_lines(idx, line, status);
        } else {
            self.set_line(idx, line, status, line);
        }
        // store the raw value (dict overwrite keeps insertion position)
        let raw = self.raw_state.entry(idx).or_default();
        match raw.iter_mut().find(|(l, _)| *l == line) {
            Some(slot) => slot.1 = status,
            None => raw.push((line, status)),
        }
    }

    /// _set_message_state_on_line (file_state.py:163-182): module state +
    /// suppression mapping maintenance.
    fn set_line(&mut self, idx: MsgIdx, line: u32, state: bool, original_lineno: u32) {
        if !state {
            self.suppression_mapping.insert((idx, line), original_lineno);
        } else {
            self.suppression_mapping.remove(&(idx, line));
        }
        self.module_state.entry(idx).or_default().insert(line, state);
    }

    /// _set_state_on_block_lines: post-order DFS; the first node whose
    /// [fromlineno, tolineno] contains the pragma line consumes it
    /// (`del lines[lineno]` makes every ancestor a no-op afterwards).
    fn set_state_on_block_lines(&mut self, idx: MsgIdx, line: u32, status: bool) {
        let consumer = *self.block_cache.entry(line).or_insert_with(|| {
            self.postorder
                .iter()
                .copied()
                .find(|&id| {
                    let n = self.tree.node(id);
                    n.fromlineno <= line && line <= n.tolineno
                })
        });
        if let Some(node) = consumer {
            self.set_message_state_in_block(node, idx, line, status);
        }
        // a pragma line past module.tolineno is contained by no node:
        // only the raw state records it (past-EOF fallback consumes it)
    }

    /// first child line number rule (_set_state_on_block_lines:83-89):
    /// Module/ClassDef/FunctionDef with non-empty body -> body[0].fromlineno,
    /// else node.tolineno. (Docstrings are doc_node, not body.)
    fn firstchildlineno(&self, id: NodeId) -> u32 {
        let body: Option<&Vec<NodeId>> = match self.tree.kind(id) {
            NodeKind::Module(d) => Some(&d.body),
            NodeKind::ClassDef(d) => Some(&d.body),
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => Some(&d.body),
            _ => None,
        };
        match body {
            Some(b) if !b.is_empty() => self.tree.node(b[0]).fromlineno,
            _ => self.tree.node(id).tolineno,
        }
    }

    fn from_to(&self, id: NodeId) -> (u32, u32) {
        let n = self.tree.node(id);
        (n.fromlineno, n.tolineno)
    }

    /// astroid `_elsed_block_range` (_base_nodes.py:244-256).
    fn elsed_block_range(&self, id: NodeId, lineno: u32, orelse: &[NodeId], last: Option<u32>) -> (u32, u32) {
        let (from, to) = self.from_to(id);
        if lineno == from {
            return (lineno, lineno);
        }
        if !orelse.is_empty() {
            let first_or = self.tree.node(orelse[0]).fromlineno;
            if lineno >= first_or {
                return (lineno, self.tree.node(*orelse.last().unwrap()).tolineno);
            }
            return (lineno, first_or - 1);
        }
        // `last or self.tolineno` -- 0/None falsy
        (lineno, last.filter(|&v| v != 0).unwrap_or(to))
    }

    /// node.block_range(lineno), per node type.
    fn block_range(&self, id: NodeId, lineno: u32) -> (u32, u32) {
        let (from, to) = self.from_to(id);
        match self.tree.kind(id) {
            // Module.block_range -> (fromlineno=0, tolineno) (scoped_nodes.py:303-310)
            NodeKind::Module(_) => (from, to),
            // FunctionDef/ClassDef -> (fromlineno, tolineno)
            NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) | NodeKind::ClassDef(_) => {
                (from, to)
            }
            // If.block_range (node_classes.py:3033-3045)
            NodeKind::If { body, orelse, .. } => {
                let body0 = self.tree.node(body[0]).fromlineno;
                if lineno == body0 {
                    return (lineno, lineno);
                }
                let body_last = self.tree.node(*body.last().unwrap()).tolineno;
                if lineno <= body_last {
                    return (lineno, body_last);
                }
                self.elsed_block_range(id, lineno, orelse, Some(body0.saturating_sub(1)))
            }
            // Try/TryStar.block_range (node_classes.py:3885-3907 / 3986-4008)
            NodeKind::Try(d) | NodeKind::TryStar(d) => {
                if lineno == from {
                    return (lineno, lineno);
                }
                if !d.body.is_empty() {
                    let b0 = self.tree.node(d.body[0]).fromlineno;
                    let bl = self.tree.node(*d.body.last().unwrap()).tolineno;
                    if b0 <= lineno && lineno <= bl {
                        return (lineno, bl);
                    }
                }
                for &h in &d.handlers {
                    if let NodeKind::ExceptHandler { type_, body, .. } = self.tree.kind(h) {
                        if let Some(t) = type_ {
                            if lineno == self.tree.node(*t).fromlineno {
                                return (lineno, lineno);
                            }
                        }
                        if !body.is_empty() {
                            let h0 = self.tree.node(body[0]).fromlineno;
                            let hl = self.tree.node(*body.last().unwrap()).tolineno;
                            if h0 <= lineno && lineno <= hl {
                                return (lineno, hl);
                            }
                        }
                    }
                }
                if !d.orelse.is_empty() {
                    let o0 = self.tree.node(d.orelse[0]).fromlineno;
                    if o0.saturating_sub(1) == lineno {
                        return (lineno, lineno);
                    }
                    let ol = self.tree.node(*d.orelse.last().unwrap()).tolineno;
                    if o0 <= lineno && lineno <= ol {
                        return (lineno, ol);
                    }
                }
                if !d.finalbody.is_empty() {
                    let f0 = self.tree.node(d.finalbody[0]).fromlineno;
                    if f0.saturating_sub(1) == lineno {
                        return (lineno, lineno);
                    }
                    let fl = self.tree.node(*d.finalbody.last().unwrap()).tolineno;
                    if f0 <= lineno && lineno <= fl {
                        return (lineno, fl);
                    }
                }
                (lineno, to)
            }
            // While.block_range -> _elsed_block_range(lineno, orelse) (node_classes.py:4444)
            NodeKind::While { orelse, .. } => self.elsed_block_range(id, lineno, orelse, None),
            // default NodeNG.block_range (node_ng.py:445-453); For/With use this
            _ => (lineno, to),
        }
    }

    /// `_set_message_state_in_block` (file_state.py:92-162) for the
    /// single-entry msg_state dict {lineno: status}.
    fn set_message_state_in_block(&mut self, node: NodeId, idx: MsgIdx, lineno: u32, status: bool) {
        let def = msgstore::def(idx);
        let node_from = self.tree.node(node).fromlineno;
        let node_to = self.tree.node(node).tolineno;
        let is_module = matches!(self.tree.kind(node), NodeKind::Module(_));
        let mut state = status;
        let (first_, last_) = if def.node_scope {
            let firstchild = self.firstchildlineno(node);
            if lineno > firstchild {
                state = true; // "late disable" rule
            }
            let (mut f, l) = self.block_range(node, lineno);
            // "if we already visited line 0 we don't need to set its state
            // again" guard (file_state.py:131-148)
            if f == node_from
                && f >= firstchild
                && self
                    .module_state
                    .get(&idx)
                    .map_or(false, |m| m.contains_key(&node_from))
            {
                f = lineno;
            }
            (f, l)
        } else {
            (lineno, node_to)
        };
        for line in first_..=last_ {
            // no-override rule for already-set prefix lines (file_state.py:153-158)
            let in_prefix = if is_module {
                node_from <= line && line < lineno
            } else {
                node_from < line && line < lineno
            };
            if in_prefix
                && self
                    .module_state
                    .get(&idx)
                    .map_or(false, |m| m.contains_key(&line))
            {
                continue;
            }
            if line == lineno {
                state = status; // "state change in the same block"
            }
            self.set_line(idx, line, state, lineno);
        }
    }

    /// FileState.handle_ignored_message (file_state.py:207-223) — only the
    /// MSG_STATE_SCOPE_MODULE branch records (the message line was disabled
    /// by an in-module pragma).
    pub fn state_scope_is_module(&self, idx: MsgIdx, line: u32) -> bool {
        self.module_state
            .get(&idx)
            .map(|m| m.contains_key(&line))
            .unwrap_or(false)
    }
}

/// pylint constants.INCOMPATIBLE_WITH_USELESS_SUPPRESSION
pub const INCOMPATIBLE_WITH_USELESS_SUPPRESSION: &[&str] =
    &["R0401", "W0402", "W1505", "W1511", "W1512", "W1513", "R0801"];

/// `_is_one_message_enabled` (message_state_handler.py:279-313).
pub fn is_message_enabled(
    global: &GlobalState,
    file_state: Option<&FileState>,
    idx: MsgIdx,
    line: Option<u32>,
) -> bool {
    let Some(line) = line else {
        return global.enabled(idx);
    };
    let Some(fs) = file_state else {
        // base FileState: empty module state, no max line
        return global.enabled(idx);
    };
    if let Some(per_line) = fs.module_state.get(&idx) {
        if let Some(&state) = per_line.get(&line) {
            return state;
        }
    }
    // past-end-of-AST fallback (max_line truthy AND line beyond it)
    if fs.max_line != 0 && line > fs.max_line {
        let mut fallback = true;
        if let Some(raw) = fs.raw_state.get(&idx) {
            // last-INSERTED entry with message_line <= line
            if let Some(&(_, en)) = raw.iter().rev().find(|(l, _)| *l <= line) {
                fallback = en;
            }
        }
        return *global.state.get(&idx).unwrap_or(&fallback);
    }
    global.enabled(idx)
}

enum Meth {
    Disable,
    Enable,
    DisableNext,
}

/// `_MessageStateHandler.process_tokens` (message_state_handler.py:347-444).
/// Returns true when the file must be ignored (`skip-file` / `disable-all` /
/// inline `disable=all`).
pub fn process_tokens(
    tokens: &[TokEvent],
    src: &SourceFile,
    file_state: &mut FileState,
    sink: &mut dyn FnMut(&mut FileState, &str, u32, String),
) -> bool {
    let mut prev_line: u32 = 0; // None
    let mut saw_newline = true;
    let mut seen_newline = true;
    for ev in tokens {
        if prev_line != 0 && prev_line != ev.row {
            saw_newline = seen_newline;
            seen_newline = false;
        }
        prev_line = ev.row;
        if ev.kind == TokEventKind::Nl {
            seen_newline = true;
        }
        if ev.kind != TokEventKind::Comment {
            continue;
        }
        let content = &src.text[ev.start as usize..ev.end as usize];
        let Some(payload) = option_po_search(content) else {
            continue;
        };
        let row = ev.row;
        let (reprs, err) = parse_pragma(&payload);
        for repr in &reprs {
            if repr.action == "disable-all" || repr.action == "skip-file" {
                if repr.action == "disable-all" {
                    sink(
                        file_state,
                        "I0022",
                        row,
                        "Pragma \"disable-all\" is deprecated, use \"skip-file\" instead"
                            .to_string(),
                    );
                }
                sink(file_state, "I0013", row, "Ignoring entire file".to_string());
                return true;
            }
            let (meth, enable) = match repr.action {
                "enable" => (Meth::Enable, true),
                "disable" => (Meth::Disable, false),
                "disable-next" => (Meth::DisableNext, false),
                "disable-msg" => {
                    sink(
                        file_state,
                        "I0022",
                        row,
                        "Pragma \"disable-msg\" is deprecated, use \"disable\" instead".to_string(),
                    );
                    (Meth::Disable, false)
                }
                "enable-msg" => {
                    sink(
                        file_state,
                        "I0022",
                        row,
                        "Pragma \"enable-msg\" is deprecated, use \"enable\" instead".to_string(),
                    );
                    (Meth::Enable, true)
                }
                _ => unreachable!(),
            };
            for msgid in &repr.messages {
                // (self._pragma_lineno bookkeeping consumed only by the
                //  format checker -- skipped)
                if repr.action == "disable" && msgid == "all" {
                    sink(
                        file_state,
                        "I0022",
                        row,
                        "Pragma \"disable=all\" is deprecated, use \"skip-file\" instead"
                            .to_string(),
                    );
                    sink(file_state, "I0013", row, "Ignoring entire file".to_string());
                    return true;
                }
                // backslash continuation: treat the two lines as one
                let l_start = if saw_newline { row } else { row - 1 };
                match get_messages_to_set(msgid, enable) {
                    Ok(defs) => {
                        for idx in defs {
                            let def = msgstore::def(idx);
                            if enable
                                && !def.enabled
                                && !msgstore::EMITTED_DISABLED_MSGIDS.contains(&def.msgid)
                            {
                                // inline enable resurrects a message we do
                                // not compute -> guaranteed false negative
                                eprintln!(
                                    "prylint: warning: inline 'enable={}' names unimplemented message {} ({}) — it cannot be emitted",
                                    msgid, def.msgid, def.symbol
                                );
                            }
                            match meth {
                                Meth::Disable | Meth::Enable => {
                                    file_state.set_msg_status(idx, l_start, enable, Scope::Module);
                                }
                                Meth::DisableNext => {
                                    file_state.set_msg_status(idx, l_start + 1, false, Scope::Line);
                                }
                            }
                            // I0011 locally-disabled (_set_one_msg_status,
                            // message_state_handler.py:160-163)
                            if !enable && def.symbol != "locally-disabled" {
                                let at = match meth {
                                    Meth::DisableNext => l_start + 1,
                                    _ => l_start,
                                };
                                sink(
                                    file_state,
                                    "I0011",
                                    at,
                                    format!("Locally disabling {} ({})", def.symbol, def.msgid),
                                );
                            }
                        }
                    }
                    Err(ResolveError::Deleted(s)) | Err(ResolveError::Moved(s)) => {
                        sink(
                            file_state,
                            "R0022",
                            row,
                            format!("Useless option value for '{}', {}", repr.action, s),
                        );
                    }
                    Err(ResolveError::Unknown) => {
                        sink(
                            file_state,
                            "W0012",
                            row,
                            format!(
                                "Unknown option value for '{}', expected a valid pylint message and got '{}'",
                                repr.action, msgid
                            ),
                        );
                    }
                }
            }
        }
        match err {
            Some(PragmaError::Unrecognized { token }) => {
                sink(
                    file_state,
                    "E0011",
                    row,
                    format!("Unrecognized file option {}", pyast::pyrepr::repr_str(&token)),
                );
            }
            Some(PragmaError::Invalid { token }) => {
                sink(
                    file_state,
                    "I0010",
                    row,
                    format!("Unable to consider inline option {}", pyast::pyrepr::repr_str(&token)),
                );
            }
            None => {}
        }
    }
    false
}
