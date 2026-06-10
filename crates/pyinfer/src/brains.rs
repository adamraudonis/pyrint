//! Inference tips (astroid brain ports): brain_builtin_inference call
//! transforms, functools.partial, str.format folding, .copy().
//! Tip caching/guard mirrors astroid/inference_tip.py:37-86.

use std::cell::RefCell;
use std::rc::Rc;

use pyast::tree::{ConstValue, IntValue, NodeKind};
use pyast::NodeId;

use crate::ctx::{copy_context, Ctx};
use crate::graph::Engine;
use crate::value::{ErrKind, Flow, GNode, GSym, SeqKind, Value, NV};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tip {
    Builtin(u8), // index into BUILTIN_NAMES
    DictFromkeys,
    CopyMethod,
    StrFormat,
    Partial,
}

const BUILTIN_NAMES: [&str; 18] = [
    "bool",
    "super",
    "callable",
    "property",
    "getattr",
    "hasattr",
    "tuple",
    "set",
    "list",
    "dict",
    "frozenset",
    "type",
    "slice",
    "isinstance",
    "issubclass",
    "len",
    "str",
    "int",
];

fn tip_id(t: Tip) -> (u8, u8) {
    match t {
        Tip::Builtin(i) => (0, i),
        Tip::DictFromkeys => (1, 0),
        Tip::CopyMethod => (2, 0),
        Tip::StrFormat => (3, 0),
        Tip::Partial => (4, 0),
    }
}

impl Engine {
    /// NodeNG._explicit_inference equivalent. None => no tip applies or
    /// UseInferenceDefault.
    pub fn explicit_inference(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let tip = self.find_tip(node)?;
        let (a, b) = tip_id(tip);
        let key = (a * 32 + b, node);
        if self.tip_guard.borrow().contains(&key) {
            return None; // recursion -> UseInferenceDefault
        }
        let cacheable = ctx.is_empty();
        if cacheable {
            if let Some(hit) = self.tip_cache.borrow().get(&key) {
                return Some(Flow::ok(hit.to_vec()));
            }
        }
        self.tip_guard.borrow_mut().insert(key);
        let res = self.run_tip(tip, node, ctx);
        self.tip_guard.borrow_mut().remove(&key);
        if let Some(flow) = &res {
            if cacheable && flow.err.is_none() {
                let mut cache = self.tip_cache.borrow_mut();
                let mut order = self.tip_order.borrow_mut();
                if cache.len() >= 64 {
                    if let Some(oldest) = order.pop_front() {
                        cache.remove(&oldest);
                    }
                }
                cache.insert(key, Rc::new(flow.vals.clone()));
                order.push_back(key);
            }
        }
        res
    }

    /// transform predicates, evaluated lazily (astroid runs them once per
    /// build; they are pure syntactic checks so this is equivalent).
    fn find_tip(&self, node: GNode) -> Option<Tip> {
        let md = self.md(node.m);
        let NodeKind::Call { func, .. } = &md.tree.nodes[node.n.idx()].kind else {
            return None;
        };
        match &md.tree.nodes[func.idx()].kind {
            NodeKind::Name { name } => {
                let n = md.tree.s(*name);
                if n == "partial" {
                    return Some(Tip::Partial);
                }
                let idx = BUILTIN_NAMES.iter().position(|&b| b == n)?;
                // re module Pattern/Match carve-out
                if n == "type" && md.name == "re" {
                    let parent = md.tree.nodes[node.n.idx()].parent;
                    if let NodeKind::Assign { targets, .. } = &md.tree.nodes[parent.idx()].kind {
                        if targets.len() == 1 {
                            if let NodeKind::AssignName { name: tn } =
                                &md.tree.nodes[targets[0].idx()].kind
                            {
                                let t = md.tree.s(*tn);
                                if t == "Pattern" || t == "Match" {
                                    return None;
                                }
                            }
                        }
                    }
                }
                Some(Tip::Builtin(idx as u8))
            }
            NodeKind::Attribute { expr, attrname, .. } => {
                let attr = md.tree.s(*attrname);
                if attr == "fromkeys" {
                    if let NodeKind::Name { name } = &md.tree.nodes[expr.idx()].kind {
                        if md.tree.s(*name) == "dict" {
                            return Some(Tip::DictFromkeys);
                        }
                    }
                    return None;
                }
                if attr == "partial" {
                    if let NodeKind::Name { name } = &md.tree.nodes[expr.idx()].kind {
                        if md.tree.s(*name) == "functools" {
                            return Some(Tip::Partial);
                        }
                    }
                    return None;
                }
                if attr == "copy" {
                    return Some(Tip::CopyMethod);
                }
                if attr == "format" {
                    // _is_str_format_call
                    let expr_g = GNode { m: node.m, n: *expr };
                    let value_is_str = match &md.tree.nodes[expr.idx()].kind {
                        NodeKind::Const(ConstValue::Str(_)) => true,
                        NodeKind::Name { .. } => matches!(
                            self.safe_infer(expr_g, &Ctx::new())
                                .and_then(|v| self.value_const(&v)),
                            Some(ConstValue::Str(_))
                        ),
                        _ => false,
                    };
                    if value_is_str {
                        return Some(Tip::StrFormat);
                    }
                    return None;
                }
                None
            }
            _ => None,
        }
    }

    fn call_parts(&self, node: GNode) -> (Vec<GNode>, Vec<(Option<GSym>, GNode)>) {
        let md = self.md(node.m);
        match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Call { args, keywords, .. } => {
                let a = args.iter().map(|&x| GNode { m: node.m, n: x }).collect();
                let kw = keywords
                    .iter()
                    .filter_map(|&k| match &md.tree.nodes[k.idx()].kind {
                        NodeKind::Keyword { arg, value } => Some((
                            arg.map(|s| self.g(&md, s)),
                            GNode { m: node.m, n: *value },
                        )),
                        _ => None,
                    })
                    .collect();
                (a, kw)
            }
            _ => (Vec::new(), Vec::new()),
        }
    }

    /// CallSite.from_call equivalent
    fn call_site_of_call(&self, node: GNode, ctx: &Rc<Ctx>) -> crate::calls::CallSite {
        let (args, kws) = self.call_parts(node);
        let cc = crate::ctx::CallCtx {
            id: self.next_callctx_id(),
            args: RefCell::new(args.into_iter().map(NV::N).collect()),
            keywords: RefCell::new(kws),
            callee: RefCell::new(None),
        };
        self.call_site_from(&cc, ctx)
    }

    fn run_tip(&self, tip: Tip, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        match tip {
            Tip::Builtin(i) => self.run_builtin_tip(BUILTIN_NAMES[i as usize], node, ctx),
            Tip::DictFromkeys => self.tip_dict_fromkeys(node, ctx),
            Tip::CopyMethod => self.tip_copy_method(node, ctx),
            Tip::StrFormat => self.tip_str_format(node, ctx),
            Tip::Partial => self.tip_partial(node, ctx),
        }
    }

    fn run_builtin_tip(&self, name: &str, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let (args, kws) = self.call_parts(node);
        match name {
            "bool" => {
                if args.len() > 1 {
                    return None;
                }
                if args.is_empty() {
                    return Some(Flow::one(Value::SynthConst(Rc::new(ConstValue::Bool(
                        false,
                    )))));
                }
                let f = self.infer(args[0], &copy_context(Some(ctx)));
                let first = match f.vals.first() {
                    Some(v) => v.clone(),
                    None => return Some(Flow::uninferable()),
                };
                if first.is_uninferable() {
                    return Some(Flow::uninferable());
                }
                match self.bool_value(&first, ctx) {
                    Some(b) => Some(Flow::one(Value::SynthConst(Rc::new(ConstValue::Bool(b))))),
                    None => Some(Flow::uninferable()),
                }
            }
            "super" => self.tip_super(node, ctx, &args),
            "callable" => {
                if args.len() != 1 {
                    return None;
                }
                let f = self.infer(args[0], &copy_context(Some(ctx)));
                let first = match f.vals.first() {
                    Some(v) => v.clone(),
                    None => return Some(Flow::uninferable()),
                };
                if first.is_uninferable() {
                    return Some(Flow::uninferable());
                }
                Some(Flow::one(Value::SynthConst(Rc::new(ConstValue::Bool(
                    self.value_callable(&first, ctx),
                )))))
            }
            "property" => {
                if args.is_empty() {
                    return None;
                }
                let f = self.infer(args[0], &copy_context(Some(ctx)));
                match f.vals.first() {
                    Some(Value::Node(g))
                        if self.kind_is(*g, |k| {
                            matches!(
                                k,
                                NodeKind::FunctionDef(_)
                                    | NodeKind::AsyncFunctionDef(_)
                                    | NodeKind::Lambda(_)
                            )
                        }) =>
                    {
                        Some(Flow::one(Value::Property { func: *g }))
                    }
                    _ => None,
                }
            }
            "getattr" => {
                let (obj, attr) = self.tip_getattr_args(&args, ctx)?;
                match (obj, attr) {
                    (Value::Uninferable, _) | (_, None) => Some(Flow::uninferable()),
                    (obj, Some(attr)) => {
                        let sym = self.sym(&attr);
                        match self.igetattr_value(&obj, sym, Some(ctx)) {
                            Ok(f) if !f.vals.is_empty() => {
                                Some(Flow::one(f.vals[0].clone()))
                            }
                            _ => {
                                if args.len() == 3 {
                                    let f = self.infer(args[2], &copy_context(Some(ctx)));
                                    f.vals.first().map(|v| Flow::one(v.clone()))
                                } else {
                                    None
                                }
                            }
                        }
                    }
                }
            }
            "hasattr" => {
                let (obj, attr) = self.tip_getattr_args(&args, ctx)?;
                match (obj, attr) {
                    (Value::Uninferable, _) | (_, None) => Some(Flow::uninferable()),
                    (obj, Some(attr)) => {
                        let sym = self.sym(&attr);
                        match self.value_getattr(&obj, sym, ctx) {
                            Ok(_) => Some(Flow::one(Value::SynthConst(Rc::new(
                                ConstValue::Bool(true),
                            )))),
                            Err(ErrKind::Attribute) => Some(Flow::one(Value::SynthConst(
                                Rc::new(ConstValue::Bool(false)),
                            ))),
                            Err(_) => Some(Flow::uninferable()),
                        }
                    }
                }
            }
            "tuple" => self.tip_container(node, &args, &kws, SeqKind::Tuple, ctx),
            "set" => self.tip_container(node, &args, &kws, SeqKind::Set, ctx),
            "list" => self.tip_container(node, &args, &kws, SeqKind::List, ctx),
            "frozenset" => {
                let f = self.tip_container(node, &args, &kws, SeqKind::Set, ctx)?;
                let mapped: Vec<Value> = f
                    .vals
                    .into_iter()
                    .map(|v| match v {
                        Value::SynthSeq { elems, .. } => Value::FrozenSet { elems },
                        other => other,
                    })
                    .collect();
                Some(Flow::ok(mapped))
            }
            "dict" => self.tip_dict(node, ctx),
            "type" => {
                if args.len() != 1 {
                    return None;
                }
                let f = self.infer(args[0], &copy_context(Some(ctx)));
                let first = f.vals.first()?.clone();
                match self.object_type(&first, ctx) {
                    Some(t) => Some(Flow::one(Value::Node(t))),
                    None => Some(Flow::uninferable()),
                }
            }
            "slice" => {
                if args.is_empty() || args.len() > 3 {
                    return None;
                }
                let mut bounds: [Option<ConstValue>; 3] = [None, None, None];
                for (i, &a) in args.iter().enumerate() {
                    let v = self.safe_infer(a, &copy_context(Some(ctx)))?;
                    let c = self.value_const(&v)?;
                    match c {
                        ConstValue::None | ConstValue::Int(_) => bounds[i] = Some(c),
                        _ => return None,
                    }
                }
                Some(Flow::one(Value::SynthSlice {
                    bounds: Rc::new(bounds),
                }))
            }
            "isinstance" | "issubclass" => self.tip_isinstance(name, node, ctx),
            "len" => {
                let site = self.call_site_of_call(node, ctx);
                if !site.keyword_arguments().is_empty() {
                    return None;
                }
                let pos = site.positional_arguments();
                if pos.len() != 1 {
                    return None;
                }
                let len = self.object_len(&pos[0], ctx)?;
                Some(Flow::one(Value::SynthConst(Rc::new(ConstValue::Int(
                    IntValue::Small(len),
                )))))
            }
            "str" => {
                let site = self.call_site_of_call(node, ctx);
                if !site.keyword_arguments().is_empty() {
                    return None;
                }
                Some(Flow::one(Value::SynthConst(Rc::new(ConstValue::Str(
                    "".into(),
                )))))
            }
            "int" => {
                let site = self.call_site_of_call(node, ctx);
                if !site.keyword_arguments().is_empty() {
                    return None;
                }
                let pos = site.positional_arguments();
                if let Some(first) = pos.first() {
                    let f = self.infer_nv(first, &copy_context(Some(ctx)));
                    let first_value = f.vals.first()?.clone();
                    if first_value.is_uninferable() {
                        return None;
                    }
                    if let Some(c) = self.value_const(&first_value) {
                        match c {
                            ConstValue::Int(_) | ConstValue::Bool(_) => {
                                let i = match c {
                                    ConstValue::Int(IntValue::Small(i)) => i,
                                    ConstValue::Bool(b) => b as i64,
                                    _ => 0,
                                };
                                return Some(Flow::one(Value::SynthConst(Rc::new(
                                    ConstValue::Int(IntValue::Small(i)),
                                ))));
                            }
                            ConstValue::Str(s) => {
                                let v = s.trim().parse::<i64>().unwrap_or(0);
                                return Some(Flow::one(Value::SynthConst(Rc::new(
                                    ConstValue::Int(IntValue::Small(v)),
                                ))));
                            }
                            _ => {}
                        }
                    }
                }
                Some(Flow::one(Value::SynthConst(Rc::new(ConstValue::Int(
                    IntValue::Small(0),
                )))))
            }
            _ => None,
        }
    }

    fn tip_getattr_args(
        &self,
        args: &[GNode],
        ctx: &Rc<Ctx>,
    ) -> Option<(Value, Option<String>)> {
        if args.len() != 2 && args.len() != 3 {
            return None;
        }
        let obj = self.infer(args[0], &copy_context(Some(ctx))).vals.first()?.clone();
        let attr = self.infer(args[1], &copy_context(Some(ctx))).vals.first()?.clone();
        if obj.is_uninferable() || attr.is_uninferable() {
            return Some((Value::Uninferable, None));
        }
        match self.value_const(&attr) {
            Some(ConstValue::Str(s)) => Some((obj, Some(s.to_string()))),
            _ => None,
        }
    }

    /// non-inferring getattr over a value (for hasattr)
    pub fn value_getattr(&self, owner: &Value, name: GSym, ctx: &Rc<Ctx>) -> Result<Vec<NV>, ErrKind> {
        match owner {
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Module(_) => self.module_getattr(g.m, name, false),
                    NodeKind::ClassDef(_) => self.class_getattr(*g, name, Some(ctx), true),
                    NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_)
                    | NodeKind::Lambda(_) => {
                        // function getattr: instance attrs + model
                        if self
                            .iattrs
                            .borrow()
                            .get(g)
                            .map(|m| m.contains_key(&name))
                            .unwrap_or(false)
                        {
                            return Ok(vec![]);
                        }
                        let names = [
                            "__name__",
                            "__doc__",
                            "__qualname__",
                            "__defaults__",
                            "__annotations__",
                            "__dict__",
                            "__kwdefaults__",
                            "__module__",
                            "__get__",
                        ];
                        if names.contains(&self.sname(name).as_str()) {
                            Ok(vec![])
                        } else {
                            Err(ErrKind::Attribute)
                        }
                    }
                    _ => self.instance_getattr(owner, name, Some(ctx), true),
                }
            }
            Value::Uninferable => Ok(vec![]),
            _ => self.instance_getattr(owner, name, Some(ctx), true),
        }
    }

    fn tip_super(&self, node: GNode, ctx: &Rc<Ctx>, args: &[GNode]) -> Option<Flow> {
        if args.len() == 1 {
            return None;
        }
        let scope = self.scope(node);
        if !self.kind_is(scope, |k| {
            matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
        }) {
            return None;
        }
        let ftype = self.func_type(scope);
        if !matches!(ftype, crate::graph::FType::Method | crate::graph::FType::ClassMethod) {
            return None;
        }
        // get_wrapping_class: nearest ClassDef frame above
        let mut cls = None;
        let mut cur = scope;
        while let Some(p) = self.parent(cur) {
            let f = self.frame(p);
            if self.kind_is(f, |k| matches!(k, NodeKind::ClassDef(_))) {
                cls = Some(f);
                break;
            }
            cur = f;
        }
        let cls = cls?;
        let (mro_pointer, mro_type): (Value, Value) = if args.is_empty() {
            let t = if ftype == crate::graph::FType::ClassMethod {
                Value::Node(cls)
            } else {
                self.instantiate_class(cls)
            };
            (Value::Node(cls), t)
        } else {
            let p = self.infer(args[0], &copy_context(Some(ctx))).vals.first()?.clone();
            let t = self.infer(args[1], &copy_context(Some(ctx))).vals.first()?.clone();
            (p, t)
        };
        if mro_pointer.is_uninferable() || mro_type.is_uninferable() {
            return None;
        }
        let pointer = match mro_pointer {
            Value::Node(g) if self.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_))) => g,
            _ => return None,
        };
        Some(Flow::one(Value::Super {
            mro_pointer: pointer,
            mro_type: Rc::new(mro_type),
            self_class: cls,
            scope,
        }))
    }

    fn tip_isinstance(&self, name: &str, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let site = self.call_site_of_call(node, ctx);
        if !site.keyword_arguments().is_empty() {
            return None;
        }
        let pos = site.positional_arguments();
        if pos.len() != 2 {
            return None;
        }
        let obj = self.infer_nv(&pos[0], &copy_context(Some(ctx))).vals.first()?.clone();
        let obj_type: GNode = if name == "isinstance" {
            self.object_type(&obj, ctx)?
        } else {
            match obj {
                Value::Node(g) if self.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_))) => g,
                _ => return None,
            }
        };
        // second arg: class or tuple of classes
        let cls_v = self.safe_infer_nv(&pos[1], ctx)?;
        let classes: Vec<Value> = match self.value_elts(&cls_v) {
            Some(elts) => elts
                .iter()
                .map(|e| match e {
                    Value::Node(g) => self
                        .safe_infer(*g, &copy_context(Some(ctx)))
                        .unwrap_or(Value::Uninferable),
                    o => o.clone(),
                })
                .collect(),
            None => vec![cls_v],
        };
        let mro = self.mro(obj_type, None).ok()?;
        for klass in &classes {
            if klass.is_uninferable() {
                return None; // AstroidTypeError -> UseInferenceDefault
            }
            if let Value::Node(kg) = klass {
                if mro.contains(kg) {
                    return Some(Flow::one(Value::SynthConst(Rc::new(ConstValue::Bool(
                        true,
                    )))));
                }
            }
        }
        Some(Flow::one(Value::SynthConst(Rc::new(ConstValue::Bool(
            false,
        )))))
    }

    fn safe_infer_nv(&self, nv: &NV, ctx: &Rc<Ctx>) -> Option<Value> {
        match nv {
            NV::N(g) => self.safe_infer(*g, &copy_context(Some(ctx))),
            NV::V(v) => Some(v.clone()),
        }
    }

    /// helpers.object_len subset
    fn object_len(&self, nv: &NV, ctx: &Rc<Ctx>) -> Option<i64> {
        let inferred = self.safe_infer_nv(nv, ctx)?;
        if inferred.is_uninferable() {
            return None;
        }
        if let Some(c) = self.value_const(&inferred) {
            return match c {
                ConstValue::Str(s) => Some(s.chars().count() as i64),
                ConstValue::Bytes(b) => Some(b.len() as i64),
                _ => None,
            };
        }
        if let Some(elts) = self.value_elts(&inferred) {
            return Some(elts.len() as i64);
        }
        if let Some(items) = self.value_dict_items(&inferred) {
            return Some(items.len() as i64);
        }
        // __len__ through the type
        let t = self.object_type(&inferred, ctx)?;
        let sym = self.sym("__len__");
        let f = self.class_igetattr(t, sym, Some(ctx), true).ok()?;
        let len_call = f.vals.first()?.clone();
        let res = self.infer_call_result(&len_call, None, Some(&copy_context(Some(ctx))));
        match res.vals.first() {
            Some(v) => match self.value_const(v) {
                Some(ConstValue::Int(IntValue::Small(i))) => Some(i),
                _ => match v {
                    Value::Inst { cls } if self.is_subtype_of(*cls, "builtins.int", None) => {
                        Some(0)
                    }
                    _ => None,
                },
            },
            None => Some(0),
        }
    }

    fn tip_container(
        &self,
        _node: GNode,
        args: &[GNode],
        _kws: &[(Option<GSym>, GNode)],
        kind: SeqKind,
        ctx: &Rc<Ctx>,
    ) -> Option<Flow> {
        if args.is_empty() {
            return Some(Flow::one(Value::SynthSeq {
                kind,
                elems: Rc::new(Vec::new()),
            }));
        }
        if args.len() > 1 {
            return None;
        }
        let arg = args[0];
        // transform on the raw node first
        let md = self.md(arg.m);
        let node_matches = match (&md.tree.nodes[arg.n.idx()].kind, kind) {
            (NodeKind::List { .. }, SeqKind::List)
            | (NodeKind::Tuple { .. }, SeqKind::Tuple)
            | (NodeKind::Set { .. }, SeqKind::Set) => true,
            _ => false,
        };
        if node_matches {
            return Some(Flow::one(Value::Node(arg)));
        }
        let transformed = self.container_transform(&Value::Node(arg), kind, ctx);
        if let Some(t) = transformed {
            return Some(Flow::one(t));
        }
        let inferred = self.infer(arg, &copy_context(Some(ctx))).vals.first()?.clone();
        if inferred.is_uninferable() {
            return None;
        }
        let transformed = self.container_transform(&inferred, kind, ctx)?;
        Some(Flow::one(transformed))
    }

    /// _container_generic_transform
    fn container_transform(&self, arg: &Value, kind: SeqKind, ctx: &Rc<Ctx>) -> Option<Value> {
        // same class -> as-is
        match (arg, kind) {
            (Value::SynthSeq { kind: k, .. }, _) if *k == kind => return Some(arg.clone()),
            (Value::Node(g), _) => {
                let md = self.md(g.m);
                let matches = match (&md.tree.nodes[g.n.idx()].kind, kind) {
                    (NodeKind::List { .. }, SeqKind::List)
                    | (NodeKind::Tuple { .. }, SeqKind::Tuple)
                    | (NodeKind::Set { .. }, SeqKind::Set) => true,
                    _ => false,
                };
                if matches {
                    return Some(arg.clone());
                }
            }
            _ => {}
        }
        // iterables
        if let Some(elts) = self.value_elts(arg) {
            let mut out = Vec::new();
            for e in elts {
                let v = match &e {
                    Value::Node(g) => self.safe_infer(*g, &copy_context(Some(ctx))),
                    other => Some(other.clone()),
                };
                if let Some(v) = v {
                    out.push(v);
                }
            }
            return Some(Value::SynthSeq {
                kind,
                elems: Rc::new(out),
            });
        }
        // dict -> keys (must be Const)
        if let Some(items) = self.value_dict_items(arg) {
            let mut out = Vec::new();
            for (k, _) in items {
                let kc = match &k {
                    Value::Node(g) => {
                        let md = self.md(g.m);
                        match &md.tree.nodes[g.n.idx()].kind {
                            NodeKind::Const(c) => Some(c.clone()),
                            _ => None,
                        }
                    }
                    Value::SynthConst(c) => Some((**c).clone()),
                    _ => None,
                }?;
                out.push(Value::SynthConst(Rc::new(kc)));
            }
            return Some(Value::SynthSeq {
                kind,
                elems: Rc::new(out),
            });
        }
        // Const str/bytes
        if let Some(c) = self.value_const(arg) {
            match c {
                ConstValue::Str(s) => {
                    return Some(Value::SynthSeq {
                        kind,
                        elems: Rc::new(
                            s.chars()
                                .map(|ch| {
                                    Value::SynthConst(Rc::new(ConstValue::Str(
                                        ch.to_string().into(),
                                    )))
                                })
                                .collect(),
                        ),
                    })
                }
                ConstValue::Bytes(b) => {
                    return Some(Value::SynthSeq {
                        kind,
                        elems: Rc::new(
                            b.iter()
                                .map(|&x| {
                                    Value::SynthConst(Rc::new(ConstValue::Int(IntValue::Small(
                                        x as i64,
                                    ))))
                                })
                                .collect(),
                        ),
                    })
                }
                _ => return None,
            }
        }
        None
    }

    fn tip_dict(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let site = self.call_site_of_call(node, ctx);
        if site.has_invalid_arguments() || site.has_invalid_keywords() {
            return None;
        }
        let args = site.positional_arguments();
        let kwargs = site.keyword_arguments();
        let mut items: Vec<(Value, Value)> = Vec::new();
        if args.is_empty() && kwargs.is_empty() {
            return Some(Flow::one(Value::SynthDict {
                items: Rc::new(items),
            }));
        }
        let kw_items = |kwargs: &[(GSym, NV)]| -> Vec<(Value, Value)> {
            kwargs
                .iter()
                .map(|(k, v)| {
                    (
                        Value::SynthConst(Rc::new(ConstValue::Str(self.sname(*k).into()))),
                        match v {
                            NV::N(g) => Value::Node(*g),
                            NV::V(val) => val.clone(),
                        },
                    )
                })
                .collect()
        };
        if !kwargs.is_empty() && args.is_empty() {
            items = kw_items(&kwargs);
        } else if args.len() == 1 {
            let elts = self.dict_arg_elts(&args[0], ctx)?;
            items = elts;
            if !kwargs.is_empty() {
                items.extend(kw_items(&kwargs));
            }
        } else {
            return None;
        }
        Some(Flow::one(Value::SynthDict {
            items: Rc::new(items),
        }))
    }

    fn dict_arg_elts(&self, arg: &NV, ctx: &Rc<Ctx>) -> Option<Vec<(Value, Value)>> {
        let inferred = self.safe_infer_nv(arg, ctx)?;
        if let Some(items) = self.value_dict_items(&inferred) {
            // each key must be Const-ish per _get_elts
            return Some(items);
        }
        let elts = self.value_elts(&inferred)?;
        let mut out = Vec::new();
        for e in elts {
            let pair = self.value_elts(&e)?;
            if pair.len() != 2 {
                return None;
            }
            out.push((pair[0].clone(), pair[1].clone()));
        }
        Some(out)
    }

    fn tip_dict_fromkeys(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let site = self.call_site_of_call(node, ctx);
        if !site.keyword_arguments().is_empty() {
            return None;
        }
        let pos = site.positional_arguments();
        if pos.is_empty() || pos.len() > 2 {
            return None;
        }
        let default = Value::SynthConst(Rc::new(ConstValue::None));
        let empty = || {
            Some(Flow::one(Value::SynthDict {
                items: Rc::new(Vec::new()),
            }))
        };
        let inferred = match &pos[0] {
            NV::N(g) => match self.infer(*g, &copy_context(Some(ctx))).vals.first() {
                Some(v) => v.clone(),
                None => return empty(),
            },
            NV::V(v) => v.clone(),
        };
        if inferred.is_uninferable() {
            return empty();
        }
        // container of Consts / str / dict keys
        let keys: Vec<Value> = if let Some(elts) = self.value_elts(&inferred) {
            for e in &elts {
                let is_const = matches!(e, Value::SynthConst(_))
                    || matches!(e, Value::Node(g)
                        if self.kind_is(*g, |k| matches!(k, NodeKind::Const(_))));
                if !is_const {
                    return empty();
                }
            }
            elts
        } else if let Some(items) = self.value_dict_items(&inferred) {
            let keys: Vec<Value> = items.iter().map(|(k, _)| k.clone()).collect();
            for e in &keys {
                let is_const = matches!(e, Value::SynthConst(_))
                    || matches!(e, Value::Node(g)
                        if self.kind_is(*g, |k| matches!(k, NodeKind::Const(_))));
                if !is_const {
                    return empty();
                }
            }
            keys
        } else if let Some(c) = self.value_const(&inferred) {
            match c {
                ConstValue::Str(s) => s
                    .chars()
                    .map(|ch| Value::SynthConst(Rc::new(ConstValue::Str(ch.to_string().into()))))
                    .collect(),
                ConstValue::Bytes(b) => b
                    .iter()
                    .map(|&x| {
                        Value::SynthConst(Rc::new(ConstValue::Int(IntValue::Small(x as i64))))
                    })
                    .collect(),
                _ => return empty(),
            }
        } else {
            return empty();
        };
        let items: Vec<(Value, Value)> = keys.into_iter().map(|k| (k, default.clone())).collect();
        Some(Flow::one(Value::SynthDict {
            items: Rc::new(items),
        }))
    }

    fn tip_copy_method(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let md = self.md(node.m);
        let expr = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Call { func, .. } => match &md.tree.nodes[func.idx()].kind {
                NodeKind::Attribute { expr, .. } => GNode { m: node.m, n: *expr },
                _ => return None,
            },
            _ => return None,
        };
        let f = self.infer(expr, &copy_context(Some(ctx)));
        if f.vals.is_empty() {
            return None;
        }
        let all_containers = f.vals.iter().all(|v| {
            matches!(
                v,
                Value::SynthDict { .. } | Value::SynthSeq { .. } | Value::FrozenSet { .. }
            ) || matches!(v, Value::Node(g)
                if self.kind_is(*g, |k| matches!(k,
                    NodeKind::Dict { .. } | NodeKind::List { .. } | NodeKind::Set { .. })))
        });
        if !all_containers {
            return None;
        }
        Some(Flow::ok(f.vals))
    }

    fn tip_str_format(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let md = self.md(node.m);
        let expr = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Call { func, .. } => match &md.tree.nodes[func.idx()].kind {
                NodeKind::Attribute { expr, .. } => GNode { m: node.m, n: *expr },
                _ => return None,
            },
            _ => return None,
        };
        let template = match &md.tree.nodes[expr.n.idx()].kind {
            NodeKind::Const(ConstValue::Str(s)) => s.to_string(),
            NodeKind::Name { .. } => match self
                .safe_infer(expr, &Ctx::new())
                .and_then(|v| self.value_const(&v))
            {
                Some(ConstValue::Str(s)) => s.to_string(),
                _ => return Some(Flow::uninferable()),
            },
            _ => return Some(Flow::uninferable()),
        };
        let site = self.call_site_of_call(node, ctx);
        let mut pos_values: Vec<String> = Vec::new();
        for p in site.positional_arguments() {
            let v = self.safe_infer_nv(&p, ctx)?;
            match self.value_const(&v) {
                Some(c) => pos_values.push(const_format_value(&c)?),
                None => return Some(Flow::uninferable()),
            }
        }
        let mut kw_values: Vec<(String, String)> = Vec::new();
        for (k, v) in site.keyword_arguments() {
            let v = self.safe_infer_nv(&v, ctx)?;
            match self.value_const(&v) {
                Some(c) => kw_values.push((self.sname(k), const_format_value(&c)?)),
                None => return Some(Flow::uninferable()),
            }
        }
        match simple_str_format(&template, &pos_values, &kw_values) {
            Some(s) => Some(Flow::one(Value::SynthConst(Rc::new(ConstValue::Str(
                s.into(),
            ))))),
            None => Some(Flow::uninferable()),
        }
    }

    fn tip_partial(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Flow> {
        let site = self.call_site_of_call(node, ctx);
        let pos = site.positional_arguments();
        if pos.is_empty() {
            return None;
        }
        let kwargs = site.keyword_arguments();
        if pos.len() == 1 && kwargs.is_empty() {
            return None;
        }
        let wrapped = match &pos[0] {
            NV::N(g) => self.infer(*g, &copy_context(Some(ctx))).vals.first().cloned(),
            NV::V(v) => Some(v.clone()),
        }?;
        let func = match wrapped {
            Value::Node(g)
                if self.kind_is(g, |k| {
                    matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
                }) =>
            {
                g
            }
            _ => return None,
        };
        // keyword names must be parameters of the wrapped function
        let spec = self.arg_spec(func);
        if let Some(spec) = &spec {
            let mut param_names: Vec<GSym> = Vec::new();
            for a in spec
                .args
                .iter()
                .chain(spec.posonlyargs.iter())
                .chain(spec.kwonlyargs.iter())
            {
                if let Some(n) = self.assign_name_of(*a) {
                    param_names.push(n);
                }
            }
            for (k, _) in &kwargs {
                if !param_names.contains(k) {
                    return None;
                }
            }
        }
        let filled_args: Vec<GNode> = pos[1..]
            .iter()
            .filter_map(|nv| match nv {
                NV::N(g) => Some(*g),
                NV::V(_) => None,
            })
            .collect();
        let filled_keywords: Vec<(GSym, GNode)> = kwargs
            .iter()
            .filter_map(|(k, v)| match v {
                NV::N(g) => Some((*k, *g)),
                NV::V(_) => None,
            })
            .collect();
        Some(Flow::one(Value::Partial {
            func,
            filled_args: Rc::new(filled_args),
            filled_keywords: Rc::new(filled_keywords),
        }))
    }
}

fn const_format_value(c: &ConstValue) -> Option<String> {
    Some(match c {
        ConstValue::Str(s) => s.to_string(),
        ConstValue::Int(IntValue::Small(i)) => i.to_string(),
        ConstValue::Int(IntValue::Big(s)) => s.to_string(),
        ConstValue::Float(f) => pyast::pyrepr::repr_float(*f),
        ConstValue::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        ConstValue::None => "None".to_string(),
        _ => return None,
    })
}

/// str.format with auto/explicit numbering and named fields (no format
/// specs beyond plain {}; specs bail to None -> Uninferable)
fn simple_str_format(
    template: &str,
    pos: &[String],
    kw: &[(String, String)],
) -> Option<String> {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    let mut auto = 0usize;
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    out.push('{');
                    continue;
                }
                let mut field = String::new();
                let mut depth = 1;
                for c2 in chars.by_ref() {
                    if c2 == '{' {
                        depth += 1;
                    } else if c2 == '}' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    field.push(c2);
                }
                if depth != 0 {
                    return None;
                }
                // no conversions / format specs / attribute access support
                if field.contains(':') || field.contains('!') || field.contains('.')
                    || field.contains('[')
                {
                    return None;
                }
                if field.is_empty() {
                    let v = pos.get(auto)?;
                    auto += 1;
                    out.push_str(v);
                } else if field.chars().all(|ch| ch.is_ascii_digit()) {
                    let i: usize = field.parse().ok()?;
                    out.push_str(pos.get(i)?);
                } else {
                    let v = kw.iter().find(|(k, _)| *k == field)?;
                    out.push_str(&v.1);
                }
            }
            '}' => {
                if chars.peek() == Some(&'}') {
                    chars.next();
                    out.push('}');
                } else {
                    return None;
                }
            }
            c => out.push(c),
        }
    }
    Some(out)
}
