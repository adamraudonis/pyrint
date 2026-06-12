//! Port of pylint/checkers/non_ascii_names.py — NonAsciiNameChecker
//! (name "nonascii-checker"): C2401 non-ascii-name, W2402
//! non-ascii-file-name, C2403 non-ascii-module-import.
//! Spec: notes/09-format.md §3 / notes/09-misc-wc.md §15.

use pyast::tree::NodeKind;
use pyinfer::value::GNode;

use crate::ckutils as u;
use crate::walker::WalkCx;

pub struct NonAsciiCk;

const T_C2401: &str = "%s name \"%s\" contains a non-ASCII character, consider renaming it.";
const T_W2402: &str = "%s name \"%s\" contains a non-ASCII character.";
const T_C2403: &str = "%s name \"%s\" contains a non-ASCII character, use an ASCII-only alias for import.";

impl NonAsciiCk {
    /// `_check_name` (non_ascii_names.py:66-85). `node_type` is the
    /// HUMAN_READABLE_TYPES key; label capitalization applied here.
    fn check_name(&self, cx: &mut WalkCx, node_type: &str, name: &str, node: GNode) {
        if name.is_ascii() {
            return;
        }
        let label = match node_type {
            "file" => "File",
            "module" => "Module",
            "const" => "Constant",
            "class" => "Class",
            "function" => "Function",
            "attr" => "Attribute",
            "argument" => "Argument",
            _ => "Variable",
        };
        let (msgid, template): (&'static str, &str) = match node_type {
            "file" => ("W2402", T_W2402),
            "module" => ("C2403", T_C2403),
            _ => ("C2401", T_C2401),
        };
        cx.emit_node(
            msgid,
            u::msg_line(cx.eng, node),
            u::msg_col(cx.eng, node),
            u::format_template(template, &[label, name]),
        );
    }

    /// visit_module [non-ascii-name, non-ascii-file-name]: checks the LAST
    /// dotted component of the astroid module name.
    pub fn visit_module(&mut self, cx: &mut WalkCx, g: GNode) {
        let name = cx.eng.md(g.m).name.clone();
        let last = name.rsplit('.').next().unwrap_or(&name).to_string();
        self.check_name(cx, "file", &last, g);
    }

    /// visit_functiondef / visit_asyncfunctiondef [non-ascii-name]
    pub fn visit_functiondef(&mut self, cx: &mut WalkCx, g: GNode) {
        let (fname, arg_nodes): (String, Vec<GNode>) = {
            let md = cx.eng.md(g.m);
            let d = match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d,
                _ => return,
            };
            let fname = match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::FunctionDef(dd) | NodeKind::AsyncFunctionDef(dd) => {
                    md.tree.s(dd.name).to_string()
                }
                _ => return,
            };
            let mut args: Vec<GNode> = Vec::new();
            if let NodeKind::Arguments(a) = &md.tree.nodes[d.args.idx()].kind {
                // posonlyargs + args + kwonlyargs; vararg/kwarg NOT checked
                for &x in a.posonlyargs.iter().chain(a.args.iter()).chain(a.kwonlyargs.iter()) {
                    args.push(GNode { m: g.m, n: x });
                }
            }
            (fname, args)
        };
        self.check_name(cx, "function", &fname, g);
        for an in arg_nodes {
            let name = match &cx.eng.md(an.m).tree.nodes[an.n.idx()].kind {
                NodeKind::AssignName { name } => cx.eng.md(an.m).tree.s(*name).to_string(),
                _ => continue,
            };
            self.check_name(cx, "argument", &name, an);
        }
    }

    /// visit_global [non-ascii-name]: each name as "const" at the Global stmt
    pub fn visit_global(&mut self, cx: &mut WalkCx, g: GNode) {
        let names: Vec<String> = match &cx.eng.md(g.m).tree.nodes[g.n.idx()].kind {
            NodeKind::Global { names } => {
                let md = cx.eng.md(g.m);
                names.iter().map(|&s| md.tree.s(s).to_string()).collect()
            }
            _ => return,
        };
        for name in names {
            self.check_name(cx, "const", &name, g);
        }
    }

    /// visit_assignname [non-ascii-name] (non_ascii_names.py:122-144)
    pub fn visit_assignname(&mut self, cx: &mut WalkCx, g: GNode) {
        let eng = cx.eng;
        let name = match &eng.md(g.m).tree.nodes[g.n.idx()].kind {
            NodeKind::AssignName { name } => eng.md(g.m).tree.s(*name).to_string(),
            _ => return,
        };
        let frame = eng.frame(g);
        let md = eng.md(frame.m);
        let node_type: &str = match &md.tree.nodes[frame.n.idx()].kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                // only direct body statements ("variable"); parameters etc.
                // (parent = Arguments) and nested targets are skipped
                let parent = match eng.parent(g) {
                    Some(p) => p,
                    None => return,
                };
                if d.body.contains(&parent.n) {
                    "variable"
                } else {
                    return;
                }
            }
            NodeKind::ClassDef(_) => "attr",
            _ => "variable", // module / lambda / comprehension targets
        };
        self.check_name(cx, node_type, &name, g);
    }

    /// visit_classdef [non-ascii-name] (non_ascii_names.py:146-151):
    /// class name, then non-inherited instance_attrs at walk ENTRY (the
    /// emission-order quirk: instance attrs report before class-body names).
    pub fn visit_classdef(&mut self, cx: &mut WalkCx, g: GNode) {
        let cname = match &cx.eng.md(g.m).tree.nodes[g.n.idx()].kind {
            NodeKind::ClassDef(d) => cx.eng.md(g.m).tree.s(d.name).to_string(),
            _ => return,
        };
        self.check_name(cx, "class", &cname, g);
        let eng = cx.eng;
        let iattrs = eng.instance_attrs_of(g); // insertion (build) order
        let ancs = eng.ancestors(g, true, None);
        for (sym, nodes) in iattrs.iter() {
            if ancs.iter().any(|&a| eng.instance_attrs_of(a).contains_key(sym)) {
                continue; // inherited attr -> skipped entirely
            }
            let Some(&first) = nodes.first() else { continue };
            let name = eng.sname(*sym);
            self.check_name(cx, "attr", &name, first);
        }
    }

    /// visit_import / visit_importfrom [non-ascii-name,
    /// non-ascii-module-import]: every alias-or-name as "module".
    pub fn visit_import(&mut self, cx: &mut WalkCx, g: GNode) {
        let names: Vec<String> = {
            let md = cx.eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::Import { names } | NodeKind::ImportFrom { names, .. } => names
                    .iter()
                    .map(|(n, alias)| md.tree.s(alias.unwrap_or(*n)).to_string())
                    .collect(),
                _ => return,
            }
        };
        for name in names {
            self.check_name(cx, "module", &name, g);
        }
    }

    /// visit_call [non-ascii-name]: keyword argument names at the Keyword
    /// node (None arg = `**expr` skipped).
    pub fn visit_call(&mut self, cx: &mut WalkCx, g: GNode) {
        let kws: Vec<(GNode, String)> = {
            let md = cx.eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::Call { keywords, .. } => keywords
                    .iter()
                    .filter_map(|&k| match &md.tree.nodes[k.idx()].kind {
                        NodeKind::Keyword { arg: Some(a), .. } => {
                            Some((GNode { m: g.m, n: k }, md.tree.s(*a).to_string()))
                        }
                        _ => None,
                    })
                    .collect(),
                _ => return,
            }
        };
        for (kn, name) in kws {
            self.check_name(cx, "argument", &name, kn);
        }
    }
}
