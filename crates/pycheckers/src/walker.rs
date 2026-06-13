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
use crate::format::FormatWalkState;
use crate::imports::ImportsChecker;
use crate::logging_ck::LoggingCk;
use crate::nonascii::NonAsciiCk;
use crate::smallck::{ChainedCompCk, DunderCallCk, LambdaExprCk, NestedMinMaxCk, ThreadingCk};
use crate::strings::StringCk;
use crate::tailmisc::{AsyncCk, DataclassCk, MatchCk, MethodArgsCk, ModIterCk, StdlibCk};
use crate::typecheck::{IterCk, TypeCk};
use crate::variables::VarsChecker;

/// prepare_checkers / only_required_for_messages gates for the full-mode
/// checkers (notes/09-format.md §0.3): a checker whose messages are ALL
/// config-disabled is DROPPED (inline `enable` cannot resurrect it); visit
/// methods behind @only_required_for_messages need one config-enabled
/// message from their list. Under `-E` every flag here is false — the new
/// callbacks never run, preserving byte-identical -E behavior.
#[derive(Default, Clone, Copy)]
pub struct Prepared {
    /// FormatChecker prepared -> visit_default registered for EVERY node
    /// (C0321; the checks_msgs gate is bypassed for visit_default)
    pub format: bool,
    /// nonascii visit_module gate: non-ascii-name | non-ascii-file-name
    pub nonascii_module: bool,
    /// nonascii name-only visit methods gate: non-ascii-name
    pub nonascii_name: bool,
    /// nonascii visit_import/importfrom gate: C2401 | C2403
    pub nonascii_import: bool,
    pub dunder: bool,
    pub threading: bool,
    pub lambda_expr: bool,
    pub nested_min_max: bool,
    pub chained: bool,
    /// StdlibChecker (DeprecatedMixin) visit_attribute gate: W4906
    pub dep_attr: bool,
    /// visit_decorators gate: W4905
    pub dep_dec: bool,
    /// stdlib mixin visit_import/visit_importfrom gate: W4901 | W4904
    pub dep_import: bool,
    /// imports mixin visit_call gate (deprecated-* messages)
    pub imports_call: bool,
    // ---- checkers/base (notes/09-basic-wc.md) ----
    /// BasicChecker kept at all (any of its messages enabled) — gates the
    /// UNDECORATED visit_try/leave_try (W0134 + _trys stack)
    pub basic_kept: bool,
    /// BasicChecker.visit_call gate: W0123|W0122|E0111|E0119|W0101
    pub basic_call: bool,
    /// visit_assert: W0129|W0199
    pub basic_assert: bool,
    /// visit_assign: W0127|W0128
    pub basic_assign: bool,
    /// visit_for (BasicChecker): W0128
    pub basic_for: bool,
    /// visit_expr: W0104|W0133|W0105|W0106|W0131
    pub basic_expr: bool,
    /// visit_lambda: W0108
    pub basic_lambda: bool,
    /// visit_dict: W0109
    pub basic_dict: bool,
    /// visit_set: W0130
    pub basic_set: bool,
    /// visit_with: W0124
    pub basic_with: bool,
    /// visit_if/visit_ifexp/visit_comprehension: W0125|W0126
    pub basic_test: bool,
    /// visit_functiondef (BasicChecker): W0102
    pub basic_fndef: bool,
    /// visit_return/visit_break: W0101|W0150
    pub basic_return: bool,
    /// visit_continue/visit_raise: W0101
    pub basic_unreach: bool,
    /// BasicErrorChecker.visit_for/visit_while: W0120
    pub erro_else: bool,
    /// PassChecker (single-message checker): W0107
    pub pass_ck: bool,
    /// ComparisonChecker.visit_compare: any of its seven messages
    pub comparison: bool,
    /// NameChecker.visit_module/classdef/functiondef: C0103|C0104
    pub name_main: bool,
    /// NameChecker.visit_assignname: C0103|C0104|C0105|C0131|C0132
    pub name_assign: bool,
    /// DocStringChecker.visit_module: C0114|C0112
    pub doc_module: bool,
    /// DocStringChecker.visit_classdef: C0115|C0112
    pub doc_class: bool,
    /// DocStringChecker.visit_functiondef: C0116|C0112
    pub doc_func: bool,
    /// FunctionChecker (single-message checker): W0135
    pub function_ck: bool,
    // ---- phase C (notes/09-variables-imports-classes-wc.md) ----
    /// full-pylint mode (not -E): unconditional full-mode computations that
    /// pylint runs regardless of message state (burns) hang off this so the
    /// -E pipeline stays byte-frozen.
    pub full: bool,
    /// ExceptionsChecker.visit_binop/visit_compare: W0716
    pub exc_op: bool,
    /// VariablesChecker.visit_global: W0601|W0602|W0603|W0604|W0622
    pub vars_global: bool,
    /// VariablesChecker.visit_for: W0644
    pub vars_for: bool,
    /// VariablesChecker.visit_const: W0611|W0612
    pub vars_const: bool,
    /// VariablesChecker.visit_excepthandler/leave_excepthandler: W0621
    pub vars_except: bool,
    /// ClassChecker.visit_assign: W0212|R0202|R0203
    pub classes_assign: bool,
}

impl Prepared {
    pub fn from_enabled(enabled: &dyn Fn(&str) -> bool, full: bool) -> Prepared {
        let any = |ids: &[&str]| ids.iter().any(|m| enabled(m));
        Prepared {
            full,
            exc_op: enabled("W0716"),
            vars_global: any(&["W0601", "W0602", "W0603", "W0604", "W0622"]),
            vars_for: enabled("W0644"),
            vars_const: any(&["W0611", "W0612"]),
            vars_except: enabled("W0621"),
            classes_assign: any(&["W0212", "R0202", "R0203"]),
            format: any(&[
                "C0301", "C0302", "C0303", "C0304", "C0305", "W0311", "W0301", "C0321", "C0325",
                "C0327", "C0328",
            ]),
            nonascii_module: any(&["C2401", "W2402"]),
            nonascii_name: enabled("C2401"),
            nonascii_import: any(&["C2401", "C2403"]),
            dunder: enabled("C2801"),
            threading: enabled("W2101"),
            lambda_expr: any(&["C3001", "C3002"]),
            nested_min_max: enabled("W3301"),
            chained: enabled("W3601"),
            dep_attr: enabled("W4906"),
            dep_dec: enabled("W4905"),
            dep_import: any(&["W4901", "W4904"]),
            imports_call: any(&["W4901", "W4902", "W4903", "W4904"]),
            basic_kept: any(&[
                "W0101", "W0102", "W0104", "W0105", "W0106", "W0108", "W0109", "W0122", "W0123",
                "W0124", "W0125", "W0126", "W0127", "W0128", "W0129", "W0130", "W0131", "W0133",
                "W0134", "W0150", "W0199", "E0111", "E0119",
            ]),
            basic_call: any(&["W0123", "W0122", "E0111", "E0119", "W0101"]),
            basic_assert: any(&["W0129", "W0199"]),
            basic_assign: any(&["W0127", "W0128"]),
            basic_for: enabled("W0128"),
            basic_expr: any(&["W0104", "W0133", "W0105", "W0106", "W0131"]),
            basic_lambda: enabled("W0108"),
            basic_dict: enabled("W0109"),
            basic_set: enabled("W0130"),
            basic_with: enabled("W0124"),
            basic_test: any(&["W0125", "W0126"]),
            basic_fndef: enabled("W0102"),
            basic_return: any(&["W0101", "W0150"]),
            basic_unreach: enabled("W0101"),
            erro_else: enabled("W0120"),
            pass_ck: enabled("W0107"),
            comparison: any(&["C0121", "C0123", "R0123", "R0124", "R0133", "W0143", "W0177"]),
            name_main: any(&["C0103", "C0104"]),
            name_assign: any(&["C0103", "C0104", "C0105", "C0131", "C0132"]),
            doc_module: any(&["C0114", "C0112"]),
            doc_class: any(&["C0115", "C0112"]),
            doc_func: any(&["C0116", "C0112"]),
            function_ck: enabled("W0135"),
        }
    }
}

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
    /// the anchor node's module, when the node may belong to ANOTHER
    /// module's tree: pylint stamps node messages with node.root().name /
    /// node.root().file (pylinter.py:1257-1263) — e.g. E0603 on __all__
    /// elements inferred through an ImportFrom (numpy.char checks flag
    /// Const nodes living in numpy._core.defchararray's tree).
    pub root_mid: Option<ModId>,
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
    /// linter.is_message_enabled(msgid) with NO line: config-level state
    /// only (pylint _is_one_message_enabled line=None path)
    pub cfg_enabled: &'a dyn Fn(&str) -> bool,
    /// full-pylint mode (not -E): gates computations pylint runs
    /// unconditionally whose burns the frozen -E pipeline never did
    pub full: bool,
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
        (self.emit)(CheckMsg { msgid, line, col, text, nodeless: false, root_mid: None });
    }
    /// node message whose anchor may live in a FOREIGN module's tree
    /// (module/path attribution from node.root(), pylinter.py:1257-1263)
    pub fn emit_node_rooted(
        &mut self,
        msgid: &'static str,
        node: GNode,
        line: u32,
        col: i64,
        text: String,
    ) {
        if self.is_crashed() {
            return;
        }
        // node.root(): walk parents to the top — REPARENT-AWARE (brain
        // extender templates re-parent top-level objects into the target
        // module, astroid brain/helpers.py:25-27)
        let mut top = node;
        while let Some(p) = self.eng.parent(top) {
            top = p;
        }
        (self.emit)(CheckMsg { msgid, line, col, text, nodeless: false, root_mid: Some(top.m) });
    }
    pub fn emit_nodeless(&mut self, msgid: &'static str, line: u32, col: i64, text: String) {
        if self.is_crashed() {
            return;
        }
        (self.emit)(CheckMsg { msgid, line, col, text, nodeless: true, root_mid: None });
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
    pub nonascii: NonAsciiCk,
    pub dunder: DunderCallCk,
    pub threading: ThreadingCk,
    pub lambda_expr: LambdaExprCk,
    pub nested: NestedMinMaxCk,
    pub chained: ChainedCompCk,
    pub format_state: FormatWalkState,
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
            basic: BasicCk::default(),
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
            nonascii: NonAsciiCk,
            dunder: DunderCallCk,
            threading: ThreadingCk,
            lambda_expr: LambdaExprCk,
            nested: NestedMinMaxCk,
            chained: ChainedCompCk,
            format_state: FormatWalkState::default(),
            caches: LintCaches::default(),
        }
    }
}

impl LintRun {
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn walk_module(
        &mut self,
        eng: &Engine,
        mid: ModId,
        prep: &Prepared,
        emit: &mut dyn FnMut(CheckMsg),
        import_oracle: &mut dyn FnMut(&str, &str) -> Option<String>,
        is_enabled: &dyn Fn(&str, u32) -> bool,
        cfg_enabled: &dyn Fn(&str) -> bool,
        add_ignored: &dyn Fn(&str, u32),
        crashed: &std::cell::Cell<bool>,
    ) {
        self.format_state.clear();
        let mut cx = WalkCx {
            eng,
            mid,
            caches: &self.caches,
            emit,
            import_oracle,
            is_enabled,
            cfg_enabled,
            full: prep.full,
            add_ignored,
            crashed,
        };
        let mut walker = Walker {
            prep: *prep,
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
            nonascii: &mut self.nonascii,
            dunder: &mut self.dunder,
            threading: &mut self.threading,
            lambda_expr: &mut self.lambda_expr,
            nested: &mut self.nested,
            chained: &mut self.chained,
            format_state: &mut self.format_state,
        };
        walker.walk(&mut cx, GNode { m: mid, n: NodeId::MODULE });
    }
}

struct Walker<'w> {
    prep: Prepared,
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
    nonascii: &'w mut NonAsciiCk,
    dunder: &'w mut DunderCallCk,
    threading: &'w mut ThreadingCk,
    lambda_expr: &'w mut LambdaExprCk,
    nested: &'w mut NestedMinMaxCk,
    chained: &'w mut ChainedCompCk,
    format_state: &'w mut FormatWalkState,
}

impl Walker<'_> {
    /// FormatChecker.visit_default (C0321) — full mode only. Its position
    /// per node class comes from the full-mode walk-order extraction
    /// (harness gen_walk_order, empty rcfile): "format" sorts between the
    /// basic/classes block and imports/..., so most tags run it before
    /// their first implemented callback; the `fmt_late` tags inline it.
    fn fmt(&mut self, cx: &mut WalkCx, g: GNode) {
        if self.prep.format {
            self.format_state.visit_default(cx, g);
        }
    }

    fn walk(&mut self, cx: &mut WalkCx, g: GNode) {
        // a crashed check visits nothing further (pylint: the exception
        // unwinds the whole ASTWalker.walk)
        if cx.is_crashed() {
            return;
        }
        // ---- visit (VISIT_ORDER in walk_order.rs; full-mode insertions
        // from the full-profile extraction) ----
        let kind_tag = {
            let md = cx.eng.md(g.m);
            kind_tag(&md.tree.nodes[g.n.idx()].kind)
        };
        let fmt_late = matches!(
            kind_tag,
            Tag::ClassDef
                | Tag::FunctionDef
                | Tag::AsyncFunctionDef
                | Tag::Assign
                | Tag::AssignAttr
                | Tag::Call
                | Tag::Compare
                | Tag::AsyncWith
                | Tag::Yield
                | Tag::YieldFrom
                | Tag::UnaryOp
                | Tag::Return
                | Tag::Break
                | Tag::Continue
                | Tag::Nonlocal
                | Tag::Starred
                | Tag::Raise
                | Tag::Try
                | Tag::TryStar
                | Tag::Attribute
                | Tag::Module
                | Tag::AssignName
                | Tag::Assert
                | Tag::Pass
                | Tag::Expr
                | Tag::If
                | Tag::IfExp
                | Tag::Comprehension
                | Tag::Lambda
                | Tag::Dict
                | Tag::Set
                | Tag::With
                | Tag::For
                | Tag::While
                | Tag::BinOp
        );
        if !fmt_late {
            self.fmt(cx, g);
        }
        match kind_tag {
            Tag::Module => {
                // full order: DocString, Name, Basic(stats only), Misdesign,
                // Format, Imports, Logging, NonAscii, Spelling, Type, Vars
                if self.prep.doc_module {
                    crate::basicwc::doc_visit_module(cx, g);
                }
                if self.prep.name_main {
                    crate::basicwc::name_visit_module(cx, g);
                }
                self.fmt(cx, g);
                self.imp.visit_module(cx, g);
                self.logging.visit_module(cx, g);
                if self.prep.nonascii_module {
                    self.nonascii.visit_module(cx, g);
                }
                self.ty.visit_module(cx, g);
                self.vars.visit_module(cx, g);
            }
            Tag::Import => {
                self.imp.visit_import(cx, g);
                self.logging.visit_import(cx, g);
                if self.prep.nonascii_import {
                    self.nonascii.visit_import(cx, g);
                }
                if self.prep.dep_import {
                    crate::deprecated::stdlib_visit_import(cx, g);
                }
            }
            Tag::ImportFrom => {
                self.imp.visit_importfrom(cx, g);
                self.logging.visit_importfrom(cx, g);
                if self.prep.nonascii_import {
                    self.nonascii.visit_import(cx, g);
                }
                if self.prep.dep_import {
                    let basename = crate::imports::importfrom_absolute_name(cx.eng, g);
                    crate::deprecated::stdlib_visit_importfrom(cx, g, &basename);
                }
            }
            Tag::ClassDef => {
                // full order: DocString, Name, Basic(stats), BasicError, ...
                if self.prep.doc_class {
                    crate::basicwc::doc_visit_classdef(cx, g);
                }
                if self.prep.name_main {
                    crate::basicwc::name_visit_classdef(cx, g);
                }
                self.basicerr.visit_classdef(cx, g);
                self.classes.visit_classdef(cx, g);
                self.fmt(cx, g);
                self.imp.visit_functiondef_family(cx, g);
                if self.prep.nonascii_name {
                    self.nonascii.visit_classdef(cx, g);
                }
                self.ty.visit_classdef(cx, g);
                self.vars.visit_classdef(cx, g);
            }
            Tag::FunctionDef => {
                // full order: Function, DocString, Name, Basic, BasicError,
                // Special, Class, Misdesign, Format, Imports, NewStyle, ...
                if self.prep.function_ck {
                    crate::basicwc::function_visit_functiondef(cx, g);
                }
                if self.prep.doc_func {
                    crate::basicwc::doc_visit_functiondef(cx, g);
                }
                if self.prep.name_main {
                    crate::basicwc::name_visit_functiondef(cx, g);
                }
                if self.prep.basic_fndef {
                    self.basic.visit_functiondef(cx, g);
                }
                self.basicerr.visit_functiondef(cx, g);
                self.special.visit_functiondef(cx, g);
                self.classes.visit_functiondef(cx, g);
                self.fmt(cx, g);
                self.imp.visit_functiondef_family(cx, g);
                self.newstyle.visit_functiondef(cx, g);
                if self.prep.nonascii_name {
                    self.nonascii.visit_functiondef(cx, g);
                }
                self.stdlib.visit_functiondef(cx, g);
                self.vars.visit_functiondef(cx, g);
            }
            Tag::AsyncFunctionDef => {
                self.async_ck.visit_asyncfunctiondef(cx, g);
                if self.prep.function_ck {
                    crate::basicwc::function_visit_functiondef(cx, g);
                }
                if self.prep.doc_func {
                    crate::basicwc::doc_visit_functiondef(cx, g);
                }
                if self.prep.name_main {
                    crate::basicwc::name_visit_functiondef(cx, g);
                }
                if self.prep.basic_fndef {
                    self.basic.visit_functiondef(cx, g);
                }
                self.basicerr.visit_functiondef(cx, g);
                self.special.visit_functiondef(cx, g);
                self.classes.visit_functiondef(cx, g);
                self.fmt(cx, g);
                self.newstyle.visit_functiondef(cx, g);
                if self.prep.nonascii_name {
                    self.nonascii.visit_functiondef(cx, g);
                }
                self.vars.visit_functiondef(cx, g);
            }
            Tag::Lambda => {
                if self.prep.basic_lambda {
                    self.basic.visit_lambda(cx, g);
                }
                self.fmt(cx, g);
                self.vars.visit_lambda(cx, g);
            }
            Tag::Comp => {
                // IterableChecker.visit_{listcomp,dictcomp,setcomp,
                // generatorexp} run BEFORE VariablesChecker
                self.iter.visit_comp(cx, g);
                self.vars.visit_comprehension_scope(cx, g);
            }
            Tag::Comprehension => {
                if self.prep.basic_test {
                    self.basic.visit_comprehension(cx, g);
                }
                self.fmt(cx, g);
                self.imp.compute_first_non_import_node(cx, g);
            }
            Tag::Name => self.vars.visit_name(cx, g),
            Tag::AssignName => {
                if self.prep.name_assign {
                    crate::basicwc::name_visit_assignname(cx, g);
                }
                self.fmt(cx, g);
                self.match_ck.visit_assignname(cx, g);
                if self.prep.nonascii_name {
                    self.nonascii.visit_assignname(cx, g);
                }
                self.vars.visit_assignname(cx, g);
            }
            Tag::DelName => self.vars.visit_delname(cx, g),
            Tag::Assign => {
                if self.prep.basic_assign {
                    self.basic.visit_assign(cx, g);
                }
                self.basicerr.visit_assign(cx, g);
                if self.prep.classes_assign {
                    self.classes.visit_assign(cx, g);
                }
                self.fmt(cx, g);
                self.imp.compute_first_non_import_node(cx, g);
                if self.prep.lambda_expr {
                    self.lambda_expr.visit_assign(cx, g);
                }
                self.ty.visit_assign(cx, g);
                self.vars.visit_assign(cx, g);
            }
            Tag::AssignAttr => {
                self.classes.visit_assignattr(cx, g);
                self.fmt(cx, g);
                self.imp.compute_first_non_import_node(cx, g);
                // TypeChecker.visit_assignattr AugAssign no-member burn:
                // E1101 disabled — skipped
            }
            Tag::Expr => {
                if self.prep.basic_expr {
                    self.basic.visit_expr(cx, g);
                }
                self.fmt(cx, g);
                self.imp.compute_first_non_import_node(cx, g);
            }
            Tag::If | Tag::IfExp => {
                if self.prep.basic_test {
                    self.basic.visit_if_test(cx, g);
                }
                self.fmt(cx, g);
                self.imp.compute_first_non_import_node(cx, g);
            }
            Tag::Call => {
                if self.prep.basic_call {
                    self.basic.visit_call(cx, g);
                }
                self.dataclass.visit_call(cx, g);
                self.fmt(cx, g);
                if self.prep.imports_call {
                    crate::deprecated::imports_visit_call(cx, g);
                }
                if self.prep.lambda_expr {
                    self.lambda_expr.visit_call(cx, g);
                }
                self.logging.visit_call(cx, g);
                self.method_args.visit_call(cx, g);
                if self.prep.nested_min_max {
                    self.nested.visit_call(cx, g);
                }
                if self.prep.nonascii_name {
                    self.nonascii.visit_call(cx, g);
                }
                self.stdlib.visit_call(cx, g);
                self.strings.visit_call(cx, g);
                self.iter.visit_call(cx, g);
                self.ty.visit_call(cx, g);
                if self.prep.dunder {
                    self.dunder.visit_call(cx, g);
                }
            }
            Tag::Await => self.ty.visit_await(cx, g),
            Tag::BinOp => {
                // full order: Misdesign, Exceptions.visit_binop, Format,
                // StringFormat, Type
                if self.prep.exc_op {
                    self.exceptions.visit_binop(cx, g);
                }
                self.fmt(cx, g);
                self.strings.visit_binop(cx, g);
                // TypeChecker.visit_binop: `|` only and dead on py3.12
                // (_py310_plus early return)
            }
            Tag::Compare => {
                if self.prep.chained {
                    self.chained.visit_compare(cx, g);
                }
                if self.prep.comparison {
                    crate::basicwc::visit_compare(cx, g);
                }
                if self.prep.exc_op {
                    self.exceptions.visit_compare(cx, g);
                }
                self.fmt(cx, g);
                self.ty.visit_compare(cx, g);
            }
            Tag::Dict => {
                if self.prep.basic_dict {
                    self.basic.visit_dict(cx, g);
                }
                self.fmt(cx, g);
                self.ty.visit_dict(cx, g);
            }
            Tag::Set => {
                if self.prep.basic_set {
                    self.basic.visit_set(cx, g);
                }
                self.fmt(cx, g);
                self.ty.visit_set(cx, g);
            }
            Tag::For => {
                // full order: Basic.visit_for, BasicError.visit_for,
                // Misdesign, Format, Imports, ModifiedIteration, ...
                if self.prep.basic_for {
                    self.basic.visit_for_basic(cx, g);
                }
                if self.prep.erro_else {
                    crate::basicwc::check_else_on_loop(cx, g);
                }
                self.fmt(cx, g);
                self.imp.visit_functiondef_family(cx, g);
                self.mod_iter.visit_for(cx, g);
                self.iter.visit_for(cx, g);
                self.ty.visit_for(cx, g);
                if self.prep.vars_for {
                    self.vars.visit_for(cx, g);
                }
            }
            Tag::While => {
                if self.prep.erro_else {
                    crate::basicwc::check_else_on_loop(cx, g);
                }
                self.fmt(cx, g);
                self.imp.visit_functiondef_family(cx, g);
            }
            Tag::AsyncFor => self.iter.visit_asyncfor(cx, g),
            Tag::AsyncWith => {
                self.async_ck.visit_asyncwith(cx, g);
                self.fmt(cx, g);
            }
            Tag::Yield => {
                self.basicerr.visit_yield(cx, g);
                self.fmt(cx, g);
            }
            Tag::YieldFrom => {
                self.basicerr.visit_yield(cx, g);
                self.fmt(cx, g);
                self.iter.visit_yieldfrom(cx, g);
            }
            Tag::Subscript => {
                self.ty.visit_subscript(cx, g);
                self.vars.visit_subscript(cx, g);
            }
            Tag::With => {
                if self.prep.basic_with {
                    self.basic.visit_with(cx, g);
                }
                self.fmt(cx, g);
                if self.prep.threading {
                    self.threading.visit_with(cx, g);
                }
                self.ty.visit_with(cx, g);
            }
            Tag::Attribute => {
                self.classes.visit_attribute(cx, g);
                self.fmt(cx, g);
                if self.prep.dep_attr {
                    crate::deprecated::visit_attribute(cx, g);
                }
            }
            Tag::UnaryOp => {
                self.basicerr.visit_unaryop(cx, g);
                self.fmt(cx, g);
                self.ty.visit_unaryop(cx, g);
            }
            Tag::Return => {
                if self.prep.basic_return {
                    self.basic.check_unreachable_stmt(cx, g);
                    self.basic.check_not_in_finally(cx, g, "return", false);
                }
                self.basicerr.visit_return(cx, g);
                self.fmt(cx, g);
            }
            Tag::Break => {
                if self.prep.basic_return {
                    self.basic.check_unreachable_stmt(cx, g);
                    self.basic.check_not_in_finally(cx, g, "break", true);
                }
                self.basicerr.visit_loopkw(cx, g, "break");
                self.fmt(cx, g);
            }
            Tag::Continue => {
                if self.prep.basic_unreach {
                    self.basic.check_unreachable_stmt(cx, g);
                }
                self.basicerr.visit_loopkw(cx, g, "continue");
                self.fmt(cx, g);
            }
            Tag::Nonlocal => {
                self.basicerr.visit_nonlocal(cx, g);
                self.fmt(cx, g);
            }
            Tag::Starred => {
                self.basicerr.visit_starred(cx, g);
                self.fmt(cx, g);
            }
            Tag::Raise => {
                if self.prep.basic_unreach {
                    self.basic.check_unreachable_stmt(cx, g);
                }
                self.exceptions.visit_raise(cx, g);
                self.fmt(cx, g);
            }
            Tag::Try => {
                if self.prep.basic_kept {
                    self.basic.visit_try(cx, g);
                }
                self.exceptions.visit_try(cx, g);
                self.fmt(cx, g);
                self.imp.compute_first_non_import_node(cx, g);
            }
            Tag::TryStar => {
                self.exceptions.visit_trystar(cx, g);
                self.fmt(cx, g);
            }
            Tag::Match => self.match_ck.visit_match(cx, g),
            Tag::MatchClass => self.match_ck.visit_matchclass(cx, g),
            Tag::Global => {
                if self.prep.nonascii_name {
                    self.nonascii.visit_global(cx, g);
                }
                if self.prep.vars_global {
                    self.vars.visit_global(cx, g);
                }
            }
            Tag::ExceptHandler => {
                // full order: Misdesign, Format, Refactoring,
                // UnsupportedVersion, Variables (fmt ran early via !fmt_late)
                if self.prep.vars_except {
                    self.vars.visit_excepthandler(cx, g);
                }
            }
            Tag::Const => {
                // full order: Misdesign, Format, Recommendation,
                // StringConstant, Ellipsis, Variables (fmt ran early)
                if self.prep.vars_const {
                    self.vars.visit_const(cx, g);
                }
            }
            Tag::NamedExpr => {
                if self.prep.lambda_expr {
                    self.lambda_expr.visit_namedexpr(cx, g);
                }
            }
            Tag::Decorators => {
                if self.prep.dep_dec {
                    crate::deprecated::visit_decorators(cx, g);
                }
            }
            Tag::Arguments => {
                // full order: Misdesign, Format(default, ran early),
                // UnsupportedVersion, Variables.visit_arguments
                if self.prep.full {
                    self.vars.visit_arguments(cx, g);
                }
            }
            Tag::Assert => {
                if self.prep.basic_assert {
                    self.basic.visit_assert(cx, g);
                }
                self.fmt(cx, g);
            }
            Tag::Pass => {
                if self.prep.pass_ck {
                    crate::basicwc::visit_pass(cx, g);
                }
                self.fmt(cx, g);
            }
            Tag::Other => {}
        }
        // ---- children ----
        let children: Vec<NodeId> = cx.eng.md(g.m).tree.children(g.n);
        for c in children {
            self.walk(cx, GNode { m: g.m, n: c });
        }
        // ---- leave (LEAVE_ORDER in walk_order.rs) ----
        // a crashed check runs NO leave callbacks (pylint: the exception
        // unwinds the whole ASTWalker.walk; per-module checker state like
        // ImportsChecker._imports_stack leaks into the NEXT module)
        if cx.is_crashed() {
            return;
        }
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
            Tag::ExceptHandler => {
                if self.prep.vars_except {
                    self.vars.leave_excepthandler(cx, g);
                }
            }
            // leave_assign/leave_with/leave_for: _store_type_annotation_names
            // (`# type:` statement comments; full-mode only)
            Tag::Assign | Tag::With | Tag::For if self.prep.full => {
                self.vars.leave_stmt_type_comment(cx, g);
            }
            Tag::Try => {
                // BasicChecker.leave_try pops _trys (crash unwinding is
                // handled by the early return above)
                if self.prep.basic_kept {
                    self.basic.trys.pop();
                }
            }
            // leave_assign/leave_with (_store_type_annotation_names): type
            // comments only — our tree drops them; no-op.
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
    Global,
    NamedExpr,
    Decorators,
    Assert,
    Pass,
    ExceptHandler,
    Const,
    Arguments,
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
        NodeKind::Global { .. } => Tag::Global,
        NodeKind::NamedExpr { .. } => Tag::NamedExpr,
        NodeKind::Decorators { .. } => Tag::Decorators,
        NodeKind::Assert { .. } => Tag::Assert,
        NodeKind::Pass => Tag::Pass,
        NodeKind::ExceptHandler { .. } => Tag::ExceptHandler,
        NodeKind::Const(_) => Tag::Const,
        NodeKind::Arguments(_) => Tag::Arguments,
        _ => Tag::Other,
    }
}
