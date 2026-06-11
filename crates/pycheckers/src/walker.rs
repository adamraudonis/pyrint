//! AST-walk driver for the ported checkers, dispatching in the EXACT pylint
//! callback order baked into walk_order.rs (only ImportsChecker and
//! VariablesChecker callbacks are live; the other checkers are not yet
//! ported and the no-op callbacks of ImportsChecker
//! (compute_first_non_import_node / visit_functiondef-family / visit_module /
//! leave_module) feed only disabled messages).
//!
//! pylint ASTWalker (pylint/utils/ast_walker.py): pre-order — visit
//! callbacks, then children in get_children() order, then leave callbacks.

use pyast::tree::NodeKind;
use pyast::NodeId;
use pyinfer::graph::Engine;
use pyinfer::value::{GNode, ModId};

use crate::ckutils as u;
use crate::ckutils::LintCaches;
use crate::imports::ImportsChecker;
use crate::variables::VarsChecker;

/// A message produced by a checker (text fully formatted; gating and
/// module/path resolution happen in the caller).
pub struct CheckMsg {
    pub msgid: &'static str,
    pub line: u32,
    pub col: i64,
    pub text: String,
    /// true for messages emitted WITHOUT a node (E0001 Cannot-import):
    /// pylint renders those under the raw (unstripped) FileItem module name
    pub nodeless: bool,
}

pub struct WalkCx<'a> {
    pub eng: &'a Engine,
    pub mid: ModId,
    pub caches: &'a LintCaches,
    pub emit: &'a mut dyn FnMut(CheckMsg),
    /// (path, resolved modname) -> exact `str(AstroidSyntaxError.error)`;
    /// None when astroid would not raise (ruff/CPython mismatch)
    pub import_oracle: &'a mut dyn FnMut(&str, &str) -> Option<String>,
}

impl WalkCx<'_> {
    pub fn emit_node(&mut self, msgid: &'static str, line: u32, col: i64, text: String) {
        (self.emit)(CheckMsg { msgid, line, col, text, nodeless: false });
    }
    pub fn emit_nodeless(&mut self, msgid: &'static str, line: u32, col: i64, text: String) {
        (self.emit)(CheckMsg { msgid, line, col, text, nodeless: true });
    }
    /// E0601/E0602/E0606 helper: message at the name node.
    pub fn emit_name_msg(&mut self, msgid: &'static str, node: GNode, name: &str) {
        let template = match msgid {
            "E0601" => "Using variable %r before assignment",
            "E0602" => "Undefined variable %r",
            "E0606" => "Possibly using variable %r before assignment",
            _ => unreachable!(),
        };
        let text = u::format_template(template, &[name]);
        let line = u::lineno(self.eng, node);
        let col = u::col_offset(self.eng, node).max(0) as i64;
        self.emit_node(msgid, line, col, text);
    }
}

/// Checker instances + caches with RUN lifetime (pylint checker instances
/// persist across modules; VariablesChecker._reported_type_checking_usage_
/// scopes is cross-module state).
pub struct LintRun {
    pub imports: ImportsChecker,
    pub vars: VarsChecker,
    pub caches: LintCaches,
}

impl Default for LintRun {
    fn default() -> Self {
        LintRun {
            imports: ImportsChecker,
            vars: VarsChecker::default(),
            caches: LintCaches::default(),
        }
    }
}

impl LintRun {
    pub fn walk_module(
        &mut self,
        eng: &Engine,
        mid: ModId,
        emit: &mut dyn FnMut(CheckMsg),
        import_oracle: &mut dyn FnMut(&str, &str) -> Option<String>,
    ) {
        let mut cx = WalkCx {
            eng,
            mid,
            caches: &self.caches,
            emit,
            import_oracle,
        };
        walk(&mut self.imports, &mut self.vars, &mut cx, GNode { m: mid, n: NodeId::MODULE });
    }
}

fn walk(imp: &mut ImportsChecker, vars: &mut VarsChecker, cx: &mut WalkCx, g: GNode) {
    // ---- visit (VISIT_ORDER in walk_order.rs) ----
    let kind_tag = {
        let md = cx.eng.md(g.m);
        kind_tag(&md.tree.nodes[g.n.idx()].kind)
    };
    match kind_tag {
        Tag::Module => {
            // ImportsChecker.visit_module: stores node.package (W-only) — no-op
            vars.visit_module(cx, g);
        }
        Tag::Import => {
            imp.visit_import(cx, g);
            // VariablesChecker.visit_import: E0611 disabled — no-op
        }
        Tag::ImportFrom => {
            imp.visit_importfrom(cx, g);
        }
        Tag::ClassDef => {
            // BasicChecker/BasicErrorChecker/ClassChecker/TypeChecker unwired;
            // ImportsChecker.visit_functiondef no-op
            vars.visit_classdef(cx, g);
        }
        Tag::FunctionDef => {
            vars.visit_functiondef(cx, g);
        }
        Tag::Lambda => vars.visit_lambda(cx, g),
        Tag::Comp => vars.visit_comprehension_scope(cx, g),
        Tag::Name => vars.visit_name(cx, g),
        Tag::AssignName => vars.visit_assignname(cx, g),
        Tag::DelName => vars.visit_delname(cx, g),
        Tag::Other => {}
    }
    // ---- children ----
    let children: Vec<NodeId> = cx.eng.md(g.m).tree.children(g.n);
    for c in children {
        walk(imp, vars, cx, GNode { m: g.m, n: c });
    }
    // ---- leave (LEAVE_ORDER in walk_order.rs) ----
    match kind_tag {
        Tag::Module => {
            // ImportsChecker.leave_module: ungrouped-imports (C) — no-op
            vars.leave_module(cx, g);
        }
        Tag::ClassDef => vars.leave_classdef(cx, g),
        Tag::FunctionDef => vars.leave_functiondef(cx, g),
        Tag::Lambda => vars.leave_lambda(cx, g),
        Tag::Comp => vars.leave_comprehension_scope(cx, g),
        _ => {}
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tag {
    Module,
    Import,
    ImportFrom,
    ClassDef,
    FunctionDef,
    Lambda,
    Comp,
    Name,
    AssignName,
    DelName,
    Other,
}

fn kind_tag(k: &NodeKind) -> Tag {
    match k {
        NodeKind::Module(_) => Tag::Module,
        NodeKind::Import { .. } => Tag::Import,
        NodeKind::ImportFrom { .. } => Tag::ImportFrom,
        NodeKind::ClassDef(_) => Tag::ClassDef,
        NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) => Tag::FunctionDef,
        NodeKind::Lambda(_) => Tag::Lambda,
        NodeKind::ListComp(_) | NodeKind::SetComp(_) | NodeKind::DictComp(_)
        | NodeKind::GeneratorExp(_) => Tag::Comp,
        NodeKind::Name { .. } => Tag::Name,
        NodeKind::AssignName { .. } => Tag::AssignName,
        NodeKind::DelName { .. } => Tag::DelName,
        _ => Tag::Other,
    }
}
