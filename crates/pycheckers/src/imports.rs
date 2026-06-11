//! ImportsChecker — in-scope subset (pylint/checkers/imports.py):
//!  - E0001 "Cannot import %r due to %s" via _get_imported_module
//!    (imports.py:1023-1053, AstroidSyntaxError branch at :1032-1036)
//!  - E0402 relative-beyond-top-level (TooManyLevelsError branch :1028-1031)
//!
//! All other messages of this checker are W/C/R (disabled under the target
//! flags) — their emission paths are NOT ported; state they maintain
//! (_first_non_import_node, import graph, ...) feeds only disabled
//! messages, so the corresponding callbacks are no-ops here
//! (compute_first_non_import_node / visit_functiondef family / leave_module
//! per walk_order.rs).

use pyast::tree::NodeKind;
use pyinfer::graph::BuildFail;
use pyinfer::value::GNode;

use crate::ckutils as u;
use crate::walker::WalkCx;

#[derive(Default)]
pub struct ImportsChecker;

impl ImportsChecker {
    /// visit_import (imports.py:528-551), in-scope parts only.
    pub fn visit_import(&mut self, cx: &mut WalkCx, node: GNode) {
        let md = cx.eng.md(node.m);
        let names: Vec<String> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Import { names } => {
                names.iter().map(|(n, _)| md.tree.s(*n).to_string()).collect()
            }
            _ => return,
        };
        drop(md);
        for name in &names {
            let _ = self.get_imported_module(cx, node, name);
        }
    }

    /// visit_importfrom (imports.py:553-579), in-scope parts only.
    pub fn visit_importfrom(&mut self, cx: &mut WalkCx, node: GNode) {
        let md = cx.eng.md(node.m);
        let basename = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::ImportFrom { modname, .. } => md.tree.s(*modname).to_string(),
            _ => return,
        };
        drop(md);
        let _ = self.get_imported_module(cx, node, &basename);
    }

    /// _get_imported_module (imports.py:1023-1053).
    fn get_imported_module(
        &mut self,
        cx: &mut WalkCx,
        importnode: GNode,
        modname: &str,
    ) -> Option<pyinfer::value::ModId> {
        match cx.eng.do_import_module(importnode, Some(modname)) {
            Ok(id) => Some(id),
            Err(BuildFail::TooManyLevels) => {
                // astroid.TooManyLevelsError (imports.py:1028-1031)
                if !ignore_import_failure(cx, importnode, modname) {
                    let line = u::lineno(cx.eng, importnode);
                    let col = u::col_offset(cx.eng, importnode).max(0) as i64;
                    cx.emit_node(
                        "E0402",
                        line,
                        col,
                        "Attempted relative import beyond top-level package".to_string(),
                    );
                }
                None
            }
            Err(BuildFail::Syntax { path, modname: resolved, .. }) => {
                // astroid.AstroidSyntaxError (imports.py:1032-1036):
                //   message = f"Cannot import {modname!r} due to '{exc.error}'"
                //   add_message("syntax-error", line=importnode.lineno, ...)
                // exc.error is the original SyntaxError; its str() embeds the
                // modname astroid resolved. The exact CPython text comes from
                // the persistent oracle keyed by (path, resolved name); a
                // None verdict means astroid would NOT raise (ruff/CPython
                // acceptance mismatch) -> emit nothing.
                if let Some(errstr) = (cx.import_oracle)(&path, &resolved) {
                    let line = u::lineno(cx.eng, importnode);
                    let text = format!(
                        "Cannot import {} due to '{}'",
                        u::py_repr_str(modname),
                        errstr
                    );
                    // node-less message: col_offset None -> 0
                    cx.emit_nodeless("E0001", line, 0, text);
                }
                None
            }
            // AstroidBuildingError branch: import-error (E0401) is disabled
            // under the target flags -> `if not is_message_enabled(...)
            // return None` fires first (imports.py:1039-1040)
            Err(BuildFail::Import(_)) => None,
        }
    }
}

/// _ignore_import_failure (imports.py:140-155). ignored-modules default ().
fn ignore_import_failure(cx: &mut WalkCx, node: GNode, _modname: &str) -> bool {
    // is_module_ignored(modname, ()) -> False with the default config
    if u::in_type_checking_block(cx.eng, cx.caches, node) {
        return true;
    }
    if let Some(parent) = cx.eng.parent(node) {
        if u::is_if(cx.eng, parent) && u::is_sys_guard(cx.eng, parent) {
            return true;
        }
    }
    u::node_ignores_exception(cx.eng, cx.caches, node, "ImportError")
}
