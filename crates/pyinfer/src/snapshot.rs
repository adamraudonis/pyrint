//! Loader for crates/pyinfer/snapshot/*.json — astroid's post-brain view of
//! builtins + C-extension stdlib modules (schema: harness/gen_snapshot.py).
//! Reconstructs a pyast `Tree` plus the scope-locals / instance-attrs maps.

use indexmap::IndexMap;
use rustc_hash::FxHashMap;
use serde_json::Value as J;

use pyast::tree::{
    ArgumentsData, ClassData, ConstValue, Ctx as ExprCtx, FunctionData, Interner, IntValue,
    ModuleData, Node, NodeKind, Tree,
};
use pyast::NodeId;

/// What an EmptyNode infers to (recorded by gen_snapshot._empty_inf).
#[derive(Debug, Clone)]
pub enum EInf {
    Uninferable,
    Const(ConstValue),
    /// (qname) — resolve through the graph lazily
    Class(String),
    Func(String),
    Inst(String),
}

/// klass.__module__/__name__ + instance flag for an EmptyNode's underlying
/// object (gen_snapshot._empty_klass) — drives the live
/// infer_ast_from_something replay (manager.py igetattr branch).
#[derive(Debug, Clone)]
pub struct EKlass {
    pub module: String,
    pub name: String,
    pub instance: bool,
}

pub struct SnapModule {
    pub tree: Tree,
    pub name: String,
    pub pure_python: bool,
    /// authoritative qnames for raw-built Class/FunctionDefs (reparenting)
    pub qnames: FxHashMap<NodeId, String>,
    /// scope node -> ordered locals
    pub locals: Vec<(NodeId, Vec<(String, Vec<NodeId>)>)>,
    /// class node -> ordered instance attrs
    pub iattrs: Vec<(NodeId, Vec<(String, Vec<NodeId>)>)>,
    /// FunctionDef.type values recorded from the live introspection
    pub ftype: FxHashMap<NodeId, String>,
    pub einf: FxHashMap<NodeId, Vec<EInf>>,
    pub eklass: FxHashMap<NodeId, EKlass>,
    /// raw-built Arguments with args=None (unknown signature)
    pub args_unknown: FxHashMap<NodeId, bool>,
}

struct Loader {
    modname: String,
    nodes: Vec<Node>,
    interner: Interner,
    idmap: FxHashMap<u64, NodeId>,
    qnames: FxHashMap<NodeId, String>,
    locals: Vec<(NodeId, Vec<(String, Vec<u64>)>)>,
    iattrs: Vec<(NodeId, Vec<(String, Vec<u64>)>)>,
    ftype: FxHashMap<NodeId, String>,
    einf: FxHashMap<NodeId, Vec<EInf>>,
    eklass: FxHashMap<NodeId, EKlass>,
    args_unknown: FxHashMap<NodeId, bool>,
}

fn const_value(v: &J) -> ConstValue {
    match v["t"].as_str().unwrap_or("none") {
        "none" => ConstValue::None,
        "ellipsis" => ConstValue::Ellipsis,
        "bool" => ConstValue::Bool(v["v"].as_bool().unwrap_or(false)),
        "int" => {
            let s = v["v"].as_str().unwrap_or("0");
            match s.parse::<i64>() {
                Ok(i) => ConstValue::Int(IntValue::Small(i)),
                Err(_) => ConstValue::Int(IntValue::Big(s.into())),
            }
        }
        "float" => ConstValue::Float(v["v"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0)),
        "complex" => ConstValue::Complex {
            real: v["re"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0),
            imag: v["im"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0),
        },
        "str" => ConstValue::Str(v["v"].as_str().unwrap_or("").into()),
        "bytes" => ConstValue::Bytes(
            v["v"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_u64().map(|b| b as u8)).collect())
                .unwrap_or_default(),
        ),
        // "other": only builtins.NotImplemented in the current snapshots
        // (census-verified) — load-bearing for the binary-operator
        // NotImplemented protocol (_base_nodes.py:646-655).
        "other" if v["repr"].as_str() == Some("NotImplemented") => ConstValue::NotImplemented,
        // "frozenset" / "tuple": value lost; none appear in the snapshots.
        _ => ConstValue::None,
    }
}

impl Loader {
    fn placeholder(&mut self) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(Node {
            kind: NodeKind::Unknown,
            parent: NodeId::MODULE,
            fromlineno: 0,
            col_offset: 0,
            end_lineno: 0,
            end_col_offset: 0,
            tolineno: 0,
        });
        id
    }

    fn load_list(&mut self, v: &J, parent: NodeId) -> Vec<NodeId> {
        match v.as_array() {
            Some(a) => a.iter().filter_map(|x| self.load(x, parent)).collect(),
            None => Vec::new(),
        }
    }

    fn load_opt(&mut self, v: &J, parent: NodeId) -> Option<NodeId> {
        if v.is_null() {
            None
        } else {
            self.load(v, parent)
        }
    }

    fn load(&mut self, v: &J, parent: NodeId) -> Option<NodeId> {
        if v.is_null() {
            return None;
        }
        // {"k":"Ref","r":i} — the SAME astroid object serialized earlier
        // (gen_snapshot.ser dedups by id()): astroid genuinely attaches one
        // node at several tree positions (builtins.type in every exception's
        // __class__ locals, OSError==IOError body re-appends), and object
        // IDENTITY is load-bearing (`cls != self` metaclass guards, set()
        // dedup, lru keys). Refs always point backwards in ser order.
        if v["k"].as_str() == Some("Ref") {
            let r = v["r"].as_u64()?;
            return Some(match self.idmap.get(&r) {
                Some(&n) => n,
                None => {
                    debug_assert!(false, "forward snapshot Ref {r}");
                    let p = self.placeholder();
                    self.idmap.insert(r, p);
                    p
                }
            });
        }
        let id = self.placeholder();
        if let Some(i) = v["i"].as_u64() {
            self.idmap.insert(i, id);
        }
        if let Some(qn) = v["qn"].as_str() {
            self.qnames.insert(id, qn.to_string());
        }
        let ch = &v["ch"];
        let kind = match v["k"].as_str().unwrap_or("") {
            "Module" => {
                let name = v["name"].as_str().unwrap_or("").to_string();
                let body = self.load_list(&ch["body"], id);
                NodeKind::Module(Box::new(ModuleData {
                    name: name.into(),
                    file: "<snapshot>".into(),
                    package: false,
                    body,
                    doc_node: None,
                    future_imports: Vec::new(),
                }))
            }
            "ClassDef" => {
                let name = self.interner.intern(v["name"].as_str().unwrap_or(""));
                let decorators = self.load_opt(&ch["decorators"], id);
                let bases = self.load_list(&ch["bases"], id);
                let keywords = self.load_list(&ch["keywords"], id);
                let body = self.load_list(&ch["body"], id);
                let type_params = self.load_list(&ch["type_params"], id);
                NodeKind::ClassDef(Box::new(ClassData {
                    name,
                    decorators,
                    bases,
                    keywords,
                    metaclass: None,
                    type_params,
                    body,
                    doc_node: self.load_opt(&ch["doc_node"], id),
                }))
            }
            "FunctionDef" => {
                let name = self.interner.intern(v["name"].as_str().unwrap_or(""));
                if let Some(ft) = v["ftype"].as_str() {
                    self.ftype.insert(id, ft.to_string());
                }
                let decorators = self.load_opt(&ch["decorators"], id);
                let args = self
                    .load_opt(&ch["args"], id)
                    .unwrap_or_else(|| self.placeholder());
                let returns = self.load_opt(&ch["returns"], id);
                let body = self.load_list(&ch["body"], id);
                let type_params = self.load_list(&ch["type_params"], id);
                NodeKind::FunctionDef(Box::new(FunctionData {
                    name,
                    decorators,
                    args,
                    returns,
                    type_params,
                    body,
                    doc_node: self.load_opt(&ch["doc_node"], id),
                }))
            }
            "Arguments" => {
                let args_null = ch["args"].is_null();
                if args_null {
                    self.args_unknown.insert(id, true);
                }
                let posonlyargs = self.load_list(&ch["posonlyargs"], id);
                let args = self.load_list(&ch["args"], id);
                let defaults = self.load_list(&ch["defaults"], id);
                let kwonlyargs = self.load_list(&ch["kwonlyargs"], id);
                let kw_defaults: Vec<Option<NodeId>> = match ch["kw_defaults"].as_array() {
                    Some(a) => a.iter().map(|x| self.load_opt(x, id)).collect(),
                    None => Vec::new(),
                };
                let n_pos = posonlyargs.len();
                let n_args = args.len();
                let n_kw = kwonlyargs.len();
                NodeKind::Arguments(Box::new(ArgumentsData {
                    posonlyargs,
                    args,
                    vararg: v["vararg"]
                        .as_str()
                        .map(|s| self.interner.intern(s)),
                    vararg_node: None,
                    kwonlyargs,
                    kwarg: v["kwarg"].as_str().map(|s| self.interner.intern(s)),
                    kwarg_node: None,
                    defaults,
                    kw_defaults,
                    annotations: vec![None; n_args],
                    posonlyargs_annotations: vec![None; n_pos],
                    kwonlyargs_annotations: vec![None; n_kw],
                    varargannotation: None,
                    kwargannotation: None,
                    tc_last_posonly: false,
                    tc_last_arg: false,
                    tc_last_kwonly: false,
                }))
            }
            "Const" => NodeKind::Const(const_value(&v["value"])),
            // str/bytes brain stub bodies (brain_builtin_inference on_bootstrap)
            "Return" => NodeKind::Return {
                value: self.load_opt(&ch["value"], id),
            },
            // live Instance values stored in locals (e.g. _io.BufferedReader.raw
            // = <FileIO instance>, raw_building object_build_class members):
            // represent as EmptyNode inferring to Instance of that class.
            "Instance" => {
                let cls = v["name"].as_str().unwrap_or("");
                let q = match v["qn"].as_str() {
                    Some(q) => q.to_string(),
                    None => format!("{}.{}", self.modname, cls),
                };
                self.einf.insert(id, vec![EInf::Inst(q)]);
                NodeKind::EmptyNode
            }
            "Name" => NodeKind::Name {
                name: self.interner.intern(v["name"].as_str().unwrap_or("")),
            },
            "AssignName" => NodeKind::AssignName {
                name: self.interner.intern(v["name"].as_str().unwrap_or("")),
            },
            "ImportFrom" => {
                let names: Vec<(pyast::tree::Sym, Option<pyast::tree::Sym>)> = v["names"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|pair| {
                                let p = pair.as_array()?;
                                let n = self.interner.intern(p.first()?.as_str()?);
                                let asn = p.get(1).and_then(|x| x.as_str());
                                Some((n, asn.map(|s| self.interner.intern(s))))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                NodeKind::ImportFrom {
                    modname: self.interner.intern(v["modname"].as_str().unwrap_or("")),
                    names,
                    level: v["level"].as_u64().map(|l| l as u32),
                }
            }
            "EmptyNode" => {
                if let Some(ek) = v["ek"].as_object() {
                    if let (Some(m), Some(n)) = (ek["mod"].as_str(), ek["name"].as_str()) {
                        self.eklass.insert(
                            id,
                            EKlass {
                                module: m.to_string(),
                                name: n.to_string(),
                                instance: ek["inst"].as_bool().unwrap_or(false),
                            },
                        );
                    }
                }
                if let Some(arr) = v["einf"].as_array() {
                    let descs = arr
                        .iter()
                        .filter_map(|d| match d["t"].as_str()? {
                            "u" => Some(EInf::Uninferable),
                            "const" => Some(EInf::Const(const_value(&d["v"]))),
                            "class" => Some(EInf::Class(d["q"].as_str()?.to_string())),
                            "func" => Some(EInf::Func(d["q"].as_str()?.to_string())),
                            "inst" => Some(EInf::Inst(d["q"].as_str()?.to_string())),
                            _ => None,
                        })
                        .collect();
                    self.einf.insert(id, descs);
                }
                NodeKind::EmptyNode
            }
            "Tuple" => NodeKind::Tuple {
                elts: self.load_list(&ch["elts"], id),
                ctx: ExprCtx::Load,
            },
            "List" => NodeKind::List {
                elts: self.load_list(&ch["elts"], id),
                ctx: ExprCtx::Load,
            },
            "Set" => NodeKind::Set {
                elts: self.load_list(&ch["elts"], id),
            },
            "Dict" => {
                let items: Vec<(NodeId, NodeId)> = ch["items"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|pair| {
                                let p = pair.as_array()?;
                                let k = self.load(p.first()?, id)?;
                                let val = self.load(p.get(1)?, id)?;
                                Some((k, val))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                NodeKind::Dict { items }
            }
            other => {
                debug_assert!(false, "unknown snapshot kind {other}");
                NodeKind::Unknown
            }
        };
        let (line, col) = match v["pos"].as_array() {
            Some(p) => (
                p.first().and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                p.get(1).and_then(|x| x.as_u64()).unwrap_or(0) as i32,
            ),
            None => (0, 0),
        };
        self.nodes[id.idx()] = Node {
            kind,
            parent,
            fromlineno: line,
            col_offset: col,
            end_lineno: 0,
            end_col_offset: -1,
            tolineno: line,
        };
        // scope locals + xtra sidecars
        if let Some(obj) = v["locals"].as_object() {
            for x in v["xtra"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
                self.load(x, id);
            }
            let entries: Vec<(String, Vec<u64>)> = obj
                .iter()
                .map(|(name, ids)| {
                    (
                        name.clone(),
                        ids.as_array()
                            .map(|a| a.iter().filter_map(|x| x.as_u64()).collect())
                            .unwrap_or_default(),
                    )
                })
                .collect();
            self.locals.push((id, entries));
        }
        if let Some(obj) = v["iattrs"].as_object() {
            for x in v["xtra_iattrs"]
                .as_array()
                .map(|a| a.as_slice())
                .unwrap_or(&[])
            {
                self.load(x, id);
            }
            let entries: Vec<(String, Vec<u64>)> = obj
                .iter()
                .map(|(name, ids)| {
                    (
                        name.clone(),
                        ids.as_array()
                            .map(|a| a.iter().filter_map(|x| x.as_u64()).collect())
                            .unwrap_or_default(),
                    )
                })
                .collect();
            if !entries.is_empty() {
                self.iattrs.push((id, entries));
            }
        }
        Some(id)
    }
}

pub fn load_snapshot(json: &str) -> Option<SnapModule> {
    let v: J = serde_json::from_str(json).ok()?;
    let modname = v["modname"].as_str().unwrap_or("").to_string();
    let mut loader = Loader {
        modname,
        nodes: Vec::new(),
        interner: Interner::default(),
        idmap: FxHashMap::default(),
        qnames: FxHashMap::default(),
        locals: Vec::new(),
        iattrs: Vec::new(),
        ftype: FxHashMap::default(),
        einf: FxHashMap::default(),
        eklass: FxHashMap::default(),
        args_unknown: FxHashMap::default(),
    };
    loader.load(&v, NodeId::MODULE)?;
    // parent fixups: a node's serialization position differs from astroid's
    // final .parent (add_local_node overwrites; last attach wins) — e.g.
    // builtins.type first serialized inside ArithmeticError's __class__
    // locals xtra, while its astroid parent is the builtins Module.
    if let Some(pf) = v["parfix"].as_object() {
        for (i, pi) in pf {
            if let (Ok(i), Some(pi)) = (i.parse::<u64>(), pi.as_u64()) {
                if let (Some(&n), Some(&p)) =
                    (loader.idmap.get(&i), loader.idmap.get(&pi))
                {
                    loader.nodes[n.idx()].parent = p;
                }
            }
        }
    }
    let resolve = |entries: Vec<(NodeId, Vec<(String, Vec<u64>)>)>,
                   idmap: &FxHashMap<u64, NodeId>| {
        entries
            .into_iter()
            .map(|(scope, names)| {
                (
                    scope,
                    names
                        .into_iter()
                        .map(|(name, ids)| {
                            (
                                name,
                                ids.iter().filter_map(|i| idmap.get(i).copied()).collect(),
                            )
                        })
                        .collect(),
                )
            })
            .collect()
    };
    let locals = resolve(loader.locals, &loader.idmap);
    let iattrs = resolve(loader.iattrs, &loader.idmap);
    let tree = Tree {
        nodes: loader.nodes,
        interner: loader.interner,
        locals: FxHashMap::default(),
        positions: FxHashMap::default(),
    };
    Some(SnapModule {
        name: v["modname"].as_str().unwrap_or("").to_string(),
        pure_python: v["pure_python"].as_bool().unwrap_or(false),
        qnames: loader.qnames,
        tree,
        locals,
        iattrs,
        ftype: loader.ftype,
        einf: loader.einf,
        eklass: loader.eklass,
        args_unknown: loader.args_unknown,
    })
}

/// IndexMap-based ordered locals used by the engine.
pub type OrderedLocals = IndexMap<u32, Vec<crate::value::GNode>>;
