//! AST-walk driver for the ported checkers, dispatching in the EXACT pylint
//! callback order baked into walk_order.rs.
//!
//! pylint ASTWalker (pylint/utils/ast_walker.py): pre-order — visit
//! callbacks, then children in get_children() order, then leave callbacks.

use pyast::tree::NodeKind;
use pyast::NodeId;
use pyinfer::graph::Engine;
use pyinfer::value::{GNode, ModId};

use crate::basicerr::{BasicCk, BasicErrCk};
use crate::ckutils as u;
use crate::ckutils::LintCaches;
use crate::classes::{ClassCk, NewStyleCk, SpecialCk};
use crate::exceptions::ExceptionsCk;
use crate::imports::ImportsChecker;
use crate::logging_ck::LoggingCk;
use crate::strings::StringCk;
use crate::tailmisc::{AsyncCk, DataclassCk, MatchCk, MethodArgsCk, ModIterCk, StdlibCk};
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
    /// linter.is_message_enabled(msgid, line) against the CURRENT module
    /// pragma state (used by _helper_string / import-order recording)
    pub is_enabled: &'a dyn Fn(&str, u32) -> bool,
    /// linter.add_ignored_message(msgid, line) — record a would-be message
    /// suppressed by checker-internal short-circuits (I0021 bookkeeping)
    pub add_ignored: &'a dyn Fn(&str, u32),
    /// set when the module check CRASHES (pylint: an exception propagates
    /// out of check_astroid_module -> AstroidError -> F0002 and every
    /// later message of the module is lost). Sources: the logging checker's
    /// bytes-format .decode() UnicodeDecodeError, and engine builds of
    /// crash-marked files (eng.crash_tripped).
    pub crashed: &'a std::cell::Cell<bool>,
}

impl WalkCx<'_> {
    pub fn is_crashed(&self) -> bool {
        self.crashed.get() || self.eng.crash_tripped()
    }
    pub fn emit_node(&mut self, msgid: &'static str, line: u32, col: i64, text: String) {
        if self.is_crashed() {
            return; // pylint: the exception already aborted the walk
        }
        (self.emit)(CheckMsg { msgid, line, col, text, nodeless: false });
    }
    pub fn emit_nodeless(&mut self, msgid: &'static str, line: u32, col: i64, text: String) {
        if self.is_crashed() {
            return;
        }
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
    pub basic: BasicCk,
    pub basicerr: BasicErrCk,
    pub exceptions: ExceptionsCk,
    pub strings: StringCk,
    pub logging: LoggingCk,
    pub async_ck: AsyncCk,
    pub match_ck: MatchCk,
    pub method_args: MethodArgsCk,
    pub dataclass: DataclassCk,
    pub mod_iter: ModIterCk,
    pub stdlib: StdlibCk,
    pub caches: LintCaches,
}

impl Default for LintRun {
    fn default() -> Self {
        LintRun {
            imports: ImportsChecker::default(),
            vars: VarsChecker::default(),
            ty: TypeCk::default(),
            iter: IterCk,
            special: SpecialCk,
            classes: ClassCk::default(),
            newstyle: NewStyleCk,
            basic: BasicCk,
            basicerr: BasicErrCk,
            exceptions: ExceptionsCk,
            strings: StringCk,
            logging: LoggingCk::default(),
            async_ck: AsyncCk,
            match_ck: MatchCk,
            method_args: MethodArgsCk,
            dataclass: DataclassCk,
            mod_iter: ModIterCk,
            stdlib: StdlibCk,
            caches: LintCaches::default(),
        }
    }
}

impl LintRun {
    #[allow(clippy::too_many_arguments)]
    pub fn walk_module(
        &mut self,
        eng: &Engine,
        mid: ModId,
        emit: &mut dyn FnMut(CheckMsg),
        import_oracle: &mut dyn FnMut(&str, &str) -> Option<String>,
        is_enabled: &dyn Fn(&str, u32) -> bool,
        add_ignored: &dyn Fn(&str, u32),
        crashed: &std::cell::Cell<bool>,
    ) {
        let mut cx = WalkCx {
            eng,
            mid,
            caches: &self.caches,
            emit,
            import_oracle,
            is_enabled,
            add_ignored,
            crashed,
        };
        let mut walker = Walker {
            imp: &mut self.imports,
            vars: &mut self.vars,
            ty: &mut self.ty,
            iter: &mut self.iter,
            special: &mut self.special,
            classes: &mut self.classes,
            newstyle: &mut self.newstyle,
            basic: &mut self.basic,
            basicerr: &mut self.basicerr,
            exceptions: &mut self.exceptions,
            strings: &mut self.strings,
            logging: &mut self.logging,
            async_ck: &mut self.async_ck,
            match_ck: &mut self.match_ck,
            method_args: &mut self.method_args,
            dataclass: &mut self.dataclass,
            mod_iter: &mut self.mod_iter,
            stdlib: &mut self.stdlib,
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
    basic: &'w mut BasicCk,
    basicerr: &'w mut BasicErrCk,
    exceptions: &'w mut ExceptionsCk,
    strings: &'w mut StringCk,
    logging: &'w mut LoggingCk,
    async_ck: &'w mut AsyncCk,
    match_ck: &'w mut MatchCk,
    method_args: &'w mut MethodArgsCk,
    dataclass: &'w mut DataclassCk,
    mod_iter: &'w mut ModIterCk,
    stdlib: &'w mut StdlibCk,
}

impl Walker<'_> {
    fn walk(&mut self, cx: &mut WalkCx, g: GNode) {
        // a crashed check visits nothing further (pylint: the exception
        // unwinds the whole ASTWalker.walk)
        if cx.is_crashed() {
            return;
        }
        // ---- visit (VISIT_ORDER in walk_order.rs) ----
        let kind_tag = {
            let md = cx.eng.md(g.m);
            kind_tag(&md.tree.nodes[g.n.idx()].kind)
        };
        match kind_tag {
            Tag::Module => {
                // BasicChecker.visit_module: stats only
                self.imp.visit_module(cx, g);
                self.logging.visit_module(cx, g);
                self.ty.visit_module(cx, g);
                self.vars.visit_module(cx, g);
            }
            Tag::Import => {
                self.imp.visit_import(cx, g);
                self.logging.visit_import(cx, g);
            }
            Tag::ImportFrom => {
                self.imp.visit_importfrom(cx, g);
                self.logging.visit_importfrom(cx, g);
            }
            Tag::ClassDef => {
                // BasicChecker.visit_classdef: stats only
                self.basicerr.visit_classdef(cx, g);
                self.classes.visit_classdef(cx, g);
                self.imp.visit_functiondef_family(cx, g);
                self.ty.visit_classdef(cx, g);
                self.vars.visit_classdef(cx, g);
            }
            Tag::FunctionDef => {
                self.basicerr.visit_functiondef(cx, g);
                self.special.visit_functiondef(cx, g);
                self.classes.visit_functiondef(cx, g);
                self.imp.visit_functiondef_family(cx, g);
                self.newstyle.visit_functiondef(cx, g);
                self.stdlib.visit_functiondef(cx, g);
                self.vars.visit_functiondef(cx, g);
            }
            Tag::AsyncFunctionDef => {
                self.async_ck.visit_asyncfunctiondef(cx, g);
                self.basicerr.visit_functiondef(cx, g);
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
            Tag::Comprehension => {
                self.imp.compute_first_non_import_node(cx, g);
            }
            Tag::Name => self.vars.visit_name(cx, g),
            Tag::AssignName => {
                self.match_ck.visit_assignname(cx, g);
                self.vars.visit_assignname(cx, g);
            }
            Tag::DelName => self.vars.visit_delname(cx, g),
            Tag::Assign => {
                self.basicerr.visit_assign(cx, g);
                self.imp.compute_first_non_import_node(cx, g);
                self.ty.visit_assign(cx, g);
                self.vars.visit_assign(cx, g);
            }
            Tag::AssignAttr => {
                self.classes.visit_assignattr(cx, g);
                self.imp.compute_first_non_import_node(cx, g);
                // TypeChecker.visit_assignattr AugAssign no-member burn:
                // E1101 disabled — skipped
            }
            Tag::Expr | Tag::If | Tag::IfExp => {
                self.imp.compute_first_non_import_node(cx, g);
            }
            Tag::Call => {
                self.basic.visit_call(cx, g);
                self.dataclass.visit_call(cx, g);
                self.logging.visit_call(cx, g);
                self.method_args.visit_call(cx, g);
                self.stdlib.visit_call(cx, g);
                self.strings.visit_call(cx, g);
                self.iter.visit_call(cx, g);
                self.ty.visit_call(cx, g);
            }
            Tag::Await => self.ty.visit_await(cx, g),
            Tag::BinOp => {
                self.strings.visit_binop(cx, g);
                // TypeChecker.visit_binop: `|` only and dead on py3.12
                // (_py310_plus early return)
            }
            Tag::Compare => self.ty.visit_compare(cx, g),
            Tag::Dict => self.ty.visit_dict(cx, g),
            Tag::Set => self.ty.visit_set(cx, g),
            Tag::For => {
                self.imp.visit_functiondef_family(cx, g);
                self.mod_iter.visit_for(cx, g);
                self.iter.visit_for(cx, g);
                self.ty.visit_for(cx, g);
            }
            Tag::While => {
                self.imp.visit_functiondef_family(cx, g);
            }
            Tag::AsyncFor => self.iter.visit_asyncfor(cx, g),
            Tag::AsyncWith => self.async_ck.visit_asyncwith(cx, g),
            Tag::Yield => self.basicerr.visit_yield(cx, g),
            Tag::YieldFrom => {
                self.basicerr.visit_yield(cx, g);
                self.iter.visit_yieldfrom(cx, g);
            }
            Tag::Subscript => {
                self.ty.visit_subscript(cx, g);
                self.vars.visit_subscript(cx, g);
            }
            Tag::With => self.ty.visit_with(cx, g),
            Tag::Attribute => self.classes.visit_attribute(cx, g),
            Tag::UnaryOp => {
                self.basicerr.visit_unaryop(cx, g);
                self.ty.visit_unaryop(cx, g);
            }
            Tag::Return => self.basicerr.visit_return(cx, g),
            Tag::Break => self.basicerr.visit_loopkw(cx, g, "break"),
            Tag::Continue => self.basicerr.visit_loopkw(cx, g, "continue"),
            Tag::Nonlocal => self.basicerr.visit_nonlocal(cx, g),
            Tag::Starred => self.basicerr.visit_starred(cx, g),
            Tag::Raise => self.exceptions.visit_raise(cx, g),
            Tag::Try => {
                self.basic.visit_try(cx, g);
                self.exceptions.visit_try(cx, g);
                self.imp.compute_first_non_import_node(cx, g);
            }
            Tag::TryStar => self.exceptions.visit_trystar(cx, g),
            Tag::Match => self.match_ck.visit_match(cx, g),
            Tag::MatchClass => self.match_ck.visit_matchclass(cx, g),
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
                self.imp.leave_module(cx, g);
                self.vars.leave_module(cx, g);
            }
            Tag::ClassDef => {
                self.classes.leave_classdef(cx, g);
                self.vars.leave_classdef(cx, g);
            }
            Tag::FunctionDef | Tag::AsyncFunctionDef => {
                self.classes.leave_functiondef(cx, g);
                self.vars.leave_functiondef(cx, g);
            }
            Tag::Lambda => self.vars.leave_lambda(cx, g),
            Tag::Comp => self.vars.leave_comprehension_scope(cx, g),
            // leave_assign/leave_with (_store_type_annotation_names): type
            // comments only — our tree drops them; no-op.
            // BasicChecker.leave_try: _trys stack for W0150 only — no-op.
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
    AsyncFunctionDef,
    Lambda,
    Comp,
    Comprehension,
    Name,
    AssignName,
    DelName,
    Assign,
    AssignAttr,
    Expr,
    If,
    IfExp,
    Call,
    Await,
    BinOp,
    Compare,
    Dict,
    Set,
    For,
    While,
    AsyncFor,
    AsyncWith,
    Yield,
    YieldFrom,
    Subscript,
    With,
    UnaryOp,
    Attribute,
    Return,
    Break,
    Continue,
    Nonlocal,
    Starred,
    Raise,
    Try,
    TryStar,
    Match,
    MatchClass,
    Other,
}

fn kind_tag(k: &NodeKind) -> Tag {
    match k {
        NodeKind::Module(_) => Tag::Module,
        NodeKind::Import { .. } => Tag::Import,
        NodeKind::ImportFrom { .. } => Tag::ImportFrom,
        NodeKind::ClassDef(_) => Tag::ClassDef,
        NodeKind::FunctionDef(_) => Tag::FunctionDef,
        NodeKind::AsyncFunctionDef(_) => Tag::AsyncFunctionDef,
        NodeKind::Lambda(_) => Tag::Lambda,
        NodeKind::ListComp(_) | NodeKind::SetComp(_) | NodeKind::DictComp(_)
        | NodeKind::GeneratorExp(_) => Tag::Comp,
        NodeKind::Comprehension { .. } => Tag::Comprehension,
        NodeKind::Name { .. } => Tag::Name,
        NodeKind::AssignName { .. } => Tag::AssignName,
        NodeKind::DelName { .. } => Tag::DelName,
        NodeKind::Assign { .. } => Tag::Assign,
        NodeKind::AssignAttr { .. } => Tag::AssignAttr,
        NodeKind::Expr { .. } => Tag::Expr,
        NodeKind::If { .. } => Tag::If,
        NodeKind::IfExp { .. } => Tag::IfExp,
        NodeKind::Call { .. } => Tag::Call,
        NodeKind::Await { .. } => Tag::Await,
        NodeKind::BinOp { .. } => Tag::BinOp,
        NodeKind::Compare { .. } => Tag::Compare,
        NodeKind::Dict { .. } => Tag::Dict,
        NodeKind::Set { .. } => Tag::Set,
        NodeKind::For(_) => Tag::For,
        NodeKind::While { .. } => Tag::While,
        NodeKind::AsyncFor(_) => Tag::AsyncFor,
        NodeKind::AsyncWith(_) => Tag::AsyncWith,
        NodeKind::Yield { .. } => Tag::Yield,
        NodeKind::YieldFrom { .. } => Tag::YieldFrom,
        NodeKind::Subscript { .. } => Tag::Subscript,
        NodeKind::With(_) => Tag::With,
        NodeKind::UnaryOp { .. } => Tag::UnaryOp,
        NodeKind::Attribute { .. } => Tag::Attribute,
        NodeKind::Return { .. } => Tag::Return,
        NodeKind::Break => Tag::Break,
        NodeKind::Continue => Tag::Continue,
        NodeKind::Nonlocal { .. } => Tag::Nonlocal,
        NodeKind::Starred { .. } => Tag::Starred,
        NodeKind::Raise { .. } => Tag::Raise,
        NodeKind::Try(_) => Tag::Try,
        NodeKind::TryStar(_) => Tag::TryStar,
        NodeKind::Match { .. } => Tag::Match,
        NodeKind::MatchClass { .. } => Tag::MatchClass,
        _ => Tag::Other,
    }
}
