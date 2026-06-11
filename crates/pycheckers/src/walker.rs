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
use crate::classes::{ClassCk, NewStyleCk, SpecialCk};
use crate::imports::ImportsChecker;
use crate::typecheck::{IterCk, TypeCk};
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
    pub ty: TypeCk,
    pub iter: IterCk,
    pub special: SpecialCk,
    pub classes: ClassCk,
    pub newstyle: NewStyleCk,
    pub caches: LintCaches,
}

impl Default for LintRun {
    fn default() -> Self {
        LintRun {
            imports: ImportsChecker,
            vars: VarsChecker::default(),
            ty: TypeCk::default(),
            iter: IterCk,
            special: SpecialCk,
            classes: ClassCk::default(),
            newstyle: NewStyleCk,
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
        let mut walker = Walker {
            imp: &mut self.imports,
            vars: &mut self.vars,
            ty: &mut self.ty,
            iter: &mut self.iter,
            special: &mut self.special,
            classes: &mut self.classes,
            newstyle: &mut self.newstyle,
        };
        walker.walk(&mut cx, GNode { m: mid, n: NodeId::MODULE });
    }
}

struct Walker<'w> {
    imp: &'w mut ImportsChecker,
    vars: &'w mut VarsChecker,
    ty: &'w mut TypeCk,
    iter: &'w mut IterCk,
    special: &'w mut SpecialCk,
    classes: &'w mut ClassCk,
    newstyle: &'w mut NewStyleCk,
}

impl Walker<'_> {
    fn walk(&mut self, cx: &mut WalkCx, g: GNode) {
        // ---- visit (VISIT_ORDER in walk_order.rs) ----
        let kind_tag = {
            let md = cx.eng.md(g.m);
            kind_tag(&md.tree.nodes[g.n.idx()].kind)
        };
        match kind_tag {
            Tag::Module => {
                // BasicChecker/ImportsChecker/LoggingChecker.visit_module: no-op
                self.ty.visit_module(cx, g);
                self.vars.visit_module(cx, g);
            }
            Tag::Import => {
                self.imp.visit_import(cx, g);
            }
            Tag::ImportFrom => {
                self.imp.visit_importfrom(cx, g);
            }
            Tag::ClassDef => {
                // BasicChecker/BasicErrorChecker unported;
                // ImportsChecker.visit_functiondef no-op
                self.classes.visit_classdef(cx, g);
                self.ty.visit_classdef(cx, g);
                self.vars.visit_classdef(cx, g);
            }
            Tag::FunctionDef => {
                // (Async)FunctionDef order: AsyncChecker/BasicErrorChecker/
                // StdlibChecker unported; ImportsChecker no-op
                self.special.visit_functiondef(cx, g);
                self.classes.visit_functiondef(cx, g);
                self.newstyle.visit_functiondef(cx, g);
                self.vars.visit_functiondef(cx, g);
            }
            Tag::Lambda => self.vars.visit_lambda(cx, g),
            Tag::Comp => {
                // IterableChecker.visit_{listcomp,dictcomp,setcomp,
                // generatorexp} run BEFORE VariablesChecker
                self.iter.visit_comp(cx, g);
                self.vars.visit_comprehension_scope(cx, g);
            }
            Tag::Name => self.vars.visit_name(cx, g),
            Tag::AssignName => self.vars.visit_assignname(cx, g),
            Tag::DelName => self.vars.visit_delname(cx, g),
            Tag::Assign => {
                // BasicErrorChecker.visit_assign / ImportsChecker.compute_
                // first_non_import_node: no-op; VariablesChecker.visit_assign
                // (E0633) unported
                self.ty.visit_assign(cx, g);
            }
            Tag::Call => {
                // BasicChecker/Dataclass/Logging/MethodArgs/Stdlib/
                // StringFormat visit_call unported
                self.iter.visit_call(cx, g);
                self.ty.visit_call(cx, g);
            }
            Tag::Await => self.ty.visit_await(cx, g),
            Tag::Compare => self.ty.visit_compare(cx, g),
            Tag::Dict => self.ty.visit_dict(cx, g),
            Tag::Set => self.ty.visit_set(cx, g),
            Tag::For => {
                // ImportsChecker.visit_functiondef no-op;
                // ModifiedIterationChecker.visit_for unported
                self.iter.visit_for(cx, g);
                self.ty.visit_for(cx, g);
            }
            Tag::AsyncFor => self.iter.visit_asyncfor(cx, g),
            Tag::YieldFrom => {
                // BasicErrorChecker.visit_yieldfrom unported
                self.iter.visit_yieldfrom(cx, g);
            }
            Tag::Subscript => {
                // VariablesChecker.visit_subscript (E0643) unported
                self.ty.visit_subscript(cx, g);
            }
            Tag::With => self.ty.visit_with(cx, g),
            Tag::Attribute => self.classes.visit_attribute(cx, g),
            Tag::AssignAttr => {
                // TypeChecker.visit_assignattr AugAssign no-member burn:
                // E1101 disabled (visit_assignattr is ungated and runs, but
                // only burns inference) — skipped
                self.classes.visit_assignattr(cx, g);
            }
            Tag::UnaryOp => {
                // BasicErrorChecker.visit_unaryop unported
                self.ty.visit_unaryop(cx, g);
            }
            Tag::Other => {}
        }
        // ---- children ----
        let children: Vec<NodeId> = cx.eng.md(g.m).tree.children(g.n);
        for c in children {
            self.walk(cx, GNode { m: g.m, n: c });
        }
        // ---- leave (LEAVE_ORDER in walk_order.rs) ----
        match kind_tag {
            Tag::Module => {
                // ImportsChecker.leave_module: ungrouped-imports (C) — no-op
                self.vars.leave_module(cx, g);
            }
            Tag::ClassDef => {
                self.classes.leave_classdef(cx, g);
                self.vars.leave_classdef(cx, g);
            }
            Tag::FunctionDef => {
                self.classes.leave_functiondef(cx, g);
                self.vars.leave_functiondef(cx, g);
            }
            Tag::Lambda => self.vars.leave_lambda(cx, g),
            Tag::Comp => self.vars.leave_comprehension_scope(cx, g),
            _ => {}
        }
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
    Assign,
    Call,
    Await,
    Compare,
    Dict,
    Set,
    For,
    AsyncFor,
    YieldFrom,
    Subscript,
    With,
    UnaryOp,
    Attribute,
    AssignAttr,
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
        NodeKind::Assign { .. } => Tag::Assign,
        NodeKind::Call { .. } => Tag::Call,
        NodeKind::Await { .. } => Tag::Await,
        NodeKind::Compare { .. } => Tag::Compare,
        NodeKind::Dict { .. } => Tag::Dict,
        NodeKind::Set { .. } => Tag::Set,
        NodeKind::For(_) => Tag::For,
        NodeKind::AsyncFor(_) => Tag::AsyncFor,
        NodeKind::YieldFrom { .. } => Tag::YieldFrom,
        NodeKind::Subscript { .. } => Tag::Subscript,
        NodeKind::With(_) => Tag::With,
        NodeKind::UnaryOp { .. } => Tag::UnaryOp,
        NodeKind::Attribute { .. } => Tag::Attribute,
        NodeKind::AssignAttr { .. } => Tag::AssignAttr,
        _ => Tag::Other,
    }
}
