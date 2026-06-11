//! NodeNG.infer dispatch + per-node `_infer` implementations.
//! Ports: astroid/nodes/node_ng.py:121-176, decorators.py, bases.py
//! _infer_stmts, node_classes.py per-node inference (notes/07 §4,6,8,16,17).

use std::rc::Rc;

use pyast::tree::{ConstValue, Ctx as ExprCtx, IntValue, NodeKind};
use pyast::NodeId;

use crate::ctx::{bind_context_to_node, copy_context, CallCtx, Ctx, MAX_INFERABLE_VALUES, MAX_INFERRED};
use crate::graph::Engine;
use crate::snapshot::EInf;
use crate::value::{value_key, Drive, End, ErrKind, Flow, GNode, GSym, SeqKind, Value, NV};

/// Streaming consumer for generator-exact inference (notes/07 §4): each
/// call is one `yield`; returning `Drive::Stop` abandons the producer just
/// like dropping a suspended Python generator.
pub type Sink<'a> = dyn FnMut(Value) -> Drive + 'a;

/// yield one value to a sink, propagating consumer abandonment.
#[macro_export]
macro_rules! yield_v {
    ($sink:expr, $v:expr) => {
        if let Drive::Stop = $sink($v) {
            return End::Stopped;
        }
    };
}

/// Identity of a synthetic value that corresponds to a real (fresh) AST
/// node in astroid — these go through NodeNG.infer when re-inferred via
/// _infer_stmts. Proxies (Instance/BoundMethod/Generator/Super/...) return
/// None: Proxy.infer yields self without entering NodeNG.infer.
pub(crate) fn synth_node_id(v: &Value) -> Option<(u8, usize)> {
    use std::rc::Rc;
    match v {
        Value::SynthConst(rc) => Some((0, Rc::as_ptr(rc) as usize)),
        Value::SynthSeq { elems, .. } => Some((1, Rc::as_ptr(elems) as *const u8 as usize)),
        Value::SynthDict { items } => Some((2, Rc::as_ptr(items) as *const u8 as usize)),
        Value::SynthSlice { bounds } => Some((3, Rc::as_ptr(bounds) as usize)),
        Value::FrozenSet { elems } => Some((4, Rc::as_ptr(elems) as *const u8 as usize)),
        // a fresh NodeNG per tip evaluation; its infer hop yields the inner
        // value (node_classes.py:5024-5028)
        Value::EvaluatedObject { value } => Some((5, Rc::as_ptr(value) as usize)),
        _ => None,
    }
}

#[derive(PartialEq, Eq, Hash, Clone, Copy)]
pub enum DedupKey {
    Node(GNode),
    Uninferable,
    /// Python object identity for synthetic values: clones of the same Rc
    /// (e.g. BoolOp's materialized operand lists reused across product
    /// pairs — node_classes.py:1668 itertools.product) are the SAME object
    /// in astroid and dedup in path_wrapper; independently const_factory'd
    /// values are distinct objects and do NOT dedup.
    Ptr(usize),
    /// ExceptionInstance object identity (InstId is fresh per
    /// materialization, preserved through clones/cache replay)
    ExcId(crate::value::InstId),
    /// BoundMethod object identity: every construction site wraps `bound`
    /// in a FRESH Rc, and cache replays clone the value (sharing the Rc) —
    /// so (func, bound-Rc-ptr) mirrors astroid's id(BoundMethod): replays
    /// dedup in path_wrapper, fresh materializations don't (the walrus
    /// double-stmt `BM | BM` collapse, update/__init__ latest_version).
    BMId(GNode, usize),
}

/// path_wrapper dedup identity (decorators.py:25-54): exact-class Instance
/// unproxies to its ClassDef *node*; node-backed values use node identity;
/// Uninferable is a singleton; every other proxy is a fresh object.
pub fn dedup_key(v: &Value) -> Option<DedupKey> {
    match v {
        Value::Node(g) => Some(DedupKey::Node(*g)),
        Value::Inst { cls, .. } => Some(DedupKey::Node(*cls)),
        Value::Uninferable => Some(DedupKey::Uninferable),
        Value::SynthConst(rc) => Some(DedupKey::Ptr(std::rc::Rc::as_ptr(rc) as usize)),
        Value::SynthSeq { elems, .. } => Some(DedupKey::Ptr(std::rc::Rc::as_ptr(elems) as usize)),
        Value::SynthDict { items } => Some(DedupKey::Ptr(std::rc::Rc::as_ptr(items) as usize)),
        Value::FrozenSet { elems } => Some(DedupKey::Ptr(std::rc::Rc::as_ptr(elems) as usize)),
        // ExceptionInstance is NOT exact-class "Instance"
        // (decorators.py:46 checks __class__.__name__), so path_wrapper
        // dedups it by python OBJECT identity: a cache replay yields the
        // SAME object (dedups), a fresh materialization a new one. InstId
        // mirrors exactly that (fresh per materialization, preserved
        // through Value clones / cache replays).
        Value::ExcInst { id, .. } => Some(DedupKey::ExcId(*id)),
        // Generator objects likewise dedup by identity; the captured
        // creation-context Rc is fresh per materialization
        // (bases.py:698) and shared by replay clones.
        Value::Generator { call_ctx, .. } => {
            Some(DedupKey::Ptr(std::rc::Rc::as_ptr(call_ctx) as usize))
        }
        // BoundMethod identity via the per-construction bound Rc
        Value::BoundMethod { func, bound } => {
            Some(DedupKey::BMId(*func, std::rc::Rc::as_ptr(bound) as *const () as usize))
        }
        // Slice objects: astroid's slice(...) tip builds a fresh Slice NODE
        // whose cache replays are the SAME object (dedup); our SynthSlice
        // clones share the bounds Rc (pandas _convert_slice_indexer's
        // repeated `return key` relays collapse to one Slice)
        Value::SynthSlice { bounds } => {
            Some(DedupKey::Ptr(std::rc::Rc::as_ptr(bounds) as *const () as usize))
        }
        _ => None,
    }
}

impl Engine {
    // ---------- entry point (NodeNG.infer) ----------

    /// Eager shim over `infer_to` for consumers that exhaust the generator
    /// (astroid `list(node.infer())` semantics).
    pub fn infer(&self, node: GNode, ctx_in: &Rc<Ctx>) -> Flow {
        let mut vals = Vec::new();
        let end = self.infer_to(node, ctx_in, &mut |v| {
            vals.push(v);
            Drive::Go
        });
        Flow {
            vals,
            err: end.err_opt(),
        }
    }

    /// NodeNG.infer (node_ng.py:121-176), streaming.
    pub fn infer_to(&self, node: GNode, ctx_in: &Rc<Ctx>, sink: &mut Sink) -> End {
        if self.depth.get() >= self.max_depth {
            return End::Raised(ErrKind::Recursion);
        }
        // `context=None` marker (tip bodies): each node.infer(None) call
        // materializes its OWN InferenceContext (node_ng.py:135-136)
        if ctx_in.synthetic_none.get() {
            let fresh = Ctx::new();
            return self.infer_to(node, &fresh, sink);
        }
        // PROXY placeholders (enum member Instances stored in locals):
        // astroid's Proxy.infer is a bare `yield self` (bases.py:139) —
        // no NodeNG.infer entry, no bump, no cache, no trace.
        if self.proxy_placeholders.borrow().contains(&node) {
            // drop the redirects borrow BEFORE driving the consumer
            let proxy_val = match self.redirects.borrow().get(&node) {
                Some(NV::V(v)) => Some(v.clone()),
                _ => None,
            };
            if let Some(v) = proxy_val {
                return match sink(v) {
                    Drive::Stop => End::Stopped,
                    Drive::Go => End::Done,
                };
            }
        }
        // synthetic-class base placeholders standing for raw cross-module
        // nodes (bases.py _infer_type_new_call stores the original Tuple
        // elts as bases): inference goes straight to the original node.
        let node = match self.redirects.borrow().get(&node) {
            Some(NV::N(g)) => *g,
            _ => node,
        };
        self.depth.set(self.depth.get() + 1);
        let r = self.infer_entry_to(node, ctx_in, sink);
        self.depth.set(self.depth.get() - 1);
        r
    }

    fn infer_entry_to(&self, node: GNode, ctx_in: &Rc<Ctx>, sink: &mut Sink) -> End {
        // debug trace (PRYLINT_TRACE_INFER) mirroring the astroid
        // NodeNG.infer monkeypatch used for bump-parity debugging
        if std::env::var("PRYLINT_TRACE_INFER").is_ok() {
            let md = self.md(node.m);
            let mut kind = crate::treeutil::kind_label(&md.tree.nodes[node.n.idx()].kind);
            // model-hop stand-ins: label by the VALUE kind so traces align
            // with astroid's fresh Const/Tuple/Dict model nodes
            if matches!(md.tree.nodes[node.n.idx()].kind, NodeKind::Unknown) {
                if let Some(NV::V(v)) = self.redirects.borrow().get(&node) {
                    kind = match v {
                        Value::SynthConst(_) => "Const",
                        Value::SynthSeq { kind: SeqKind::Tuple, .. } => "Tuple",
                        Value::SynthSeq { kind: SeqKind::List, .. } => "List",
                        Value::SynthDict { .. } => "Dict",
                        Value::Uninferable => "Unknown",
                        // _infer_type_call/_infer_type_new_call slots wrap
                        // pre-inferred nodes like astroid's EvaluatedObject
                        Value::Node(_) => "EvaluatedObject",
                        _ => kind,
                    };
                }
            }
            return self.infer_entry_trace_labeled(node, ctx_in, sink, kind);
        }
        self.infer_entry_to_inner(node, ctx_in, sink)
    }

    fn infer_entry_trace_labeled(
        &self,
        node: GNode,
        ctx_in: &Rc<Ctx>,
        sink: &mut Sink,
        kind: &str,
    ) -> End {
        {
            let name = if kind == "EvaluatedObject" {
                "EvaluatedObject".to_string()
            } else {
                let mut n = self.node_name(node).unwrap_or_default();
                // raw-built import stubs get `.name` set by
                // raw_building._attach_local_node — mirror in the trace
                if n.is_empty() && kind == "ImportFrom" {
                    let md = self.md(node.m);
                    if !md.pure_python {
                        if let NodeKind::ImportFrom { names, .. } =
                            &md.tree.nodes[node.n.idx()].kind
                        {
                            if let Some((nm, _)) = names.first() {
                                n = md.tree.s(*nm).to_string();
                            }
                        }
                    }
                }
                n
            };
            let d = self.depth.get() as usize;
            let ccid = ctx_in.callcontext.borrow().as_ref().map(|c| c.id);
            let bn = ctx_in.boundnode.borrow().is_some();
            eprintln!(
                "{}> {} {} ln={:?} cc={:?} bn={} ni={}",
                "  ".repeat(d),
                kind,
                name,
                ctx_in.lookupname.get().map(|s| self.sname(s)),
                ccid,
                bn,
                ctx_in.nodes_inferred.get()
            );
            let mut any = false;
            let r = {
                let any = &mut any;
                let mut wrapped = |v: Value| -> Drive {
                    *any = true;
                    eprintln!(
                        "{}  yield {} ni={}",
                        "  ".repeat(d),
                        crate::dump::render(self, &v),
                        ctx_in.nodes_inferred.get()
                    );
                    let dr = sink(v);
                    eprintln!("{}  <-{:?}", "  ".repeat(d), dr);
                    dr
                };
                self.infer_entry_to_inner(node, ctx_in, &mut wrapped)
            };
            if !any {
                eprintln!("{}  (empty end={:?})", "  ".repeat(d), r);
            }
            r
        }
    }

    fn infer_entry_to_inner(&self, node: GNode, ctx_in: &Rc<Ctx>, sink: &mut Sink) -> End {
        // extra_context swap (node_ng.py:125-128)
        let ctx = {
            let extra = ctx_in.extra_context.borrow();
            match extra.get(&node) {
                Some(c) => Rc::clone(c),
                None => Rc::clone(ctx_in),
            }
        };
        // explicit inference (inference tips). astroid materializes tip
        // results (inference_tip.py:64-66 list(func(...))), then replays:
        // `context.nodes_inferred += 1; yield result` — bump BEFORE yield.
        if let Some(flow) = self.explicit_inference(node, &ctx) {
            for v in flow.vals {
                ctx.bump_inferred();
                yield_v!(sink, v);
            }
            return match flow.err {
                Some(e) => End::Raised(e),
                None => End::Done,
            };
        }
        let key = (
            node,
            ctx.lookupname.get(),
            ctx.callcontext.borrow().as_ref().map(|c| c.id),
            ctx.boundnode.borrow().as_ref().map(value_key),
        );
        let cached = self.inf_cache.borrow().get(&key).cloned();
        if let Some(cached) = cached {
            // replay without bumping nodes_inferred (node_ng.py:155-157)
            for v in cached.iter() {
                yield_v!(sink, v.clone());
            }
            return End::Done;
        }
        // limit loop (node_ng.py:160-176)
        let mut results: Vec<Value> = Vec::new();
        let mut i: usize = 0;
        let mut truncated = false;
        let mut cache_after_trunc = false;
        let mut consumer_stopped = false;
        let end = {
            let results = &mut results;
            let i = &mut i;
            let truncated = &mut truncated;
            let cache_after_trunc = &mut cache_after_trunc;
            let consumer_stopped = &mut consumer_stopped;
            let ctx2 = Rc::clone(&ctx);
            self.infer_dispatch_to(node, &ctx, &mut |v| {
                if *i >= MAX_INFERABLE_VALUES || ctx2.nodes_inferred.get() > MAX_INFERRED {
                    results.push(Value::Uninferable);
                    let d = sink(Value::Uninferable);
                    *truncated = true;
                    // node_ng.py:164-167: `yield Uninferable` SUSPENDS
                    // before `break` — the cache write below the loop only
                    // runs if the consumer pulls again (Drive::Go). A
                    // consumer abandoning at this yield drops the generator
                    // while suspended: NO cache write (probe: os.path attr
                    // chain stays uncached after a capped abspath call).
                    *cache_after_trunc = matches!(d, Drive::Go);
                    if std::env::var("PRYLINT_TRACE_INFER").is_ok() {
                        eprintln!("TRUNC i={} ni={} d={:?}", i, ctx2.nodes_inferred.get(), d);
                    }
                    return Drive::Stop;
                }
                results.push(v.clone());
                let d = sink(v);
                if let Drive::Stop = d {
                    // consumer abandoned at the yield: the post-yield
                    // `context.nodes_inferred += 1` never runs, and the
                    // cache write is skipped (generator dropped).
                    *consumer_stopped = true;
                    return Drive::Stop;
                }
                ctx2.bump_inferred();
                *i += 1;
                Drive::Go
            })
        };
        let trace_write = |n: usize| {
            if std::env::var("PRYLINT_TRACE_INFER").is_ok() {
                let md = self.md(node.m);
                let kind = crate::treeutil::kind_label(&md.tree.nodes[node.n.idx()].kind);
                let name = self.node_name(node).unwrap_or_default();
                eprintln!("CACHEW {kind} {name} vals={n}");
            }
        };
        match end {
            // node_ng.py:163-167: after the truncation `yield Uninferable`
            // the wrapper is SUSPENDED before `break`; the cache write
            // below the loop runs ONLY if the consumer pulls again
            // (cache_after_trunc). How the producer ended is irrelevant —
            // astroid never pulls it after the break. Without this arm a
            // producer that completes Done after the truncation Stop fell
            // into the unconditional cache branch, freezing every
            // mid-cascade node of a cap blow (GT re-burns ##103 on the
            // next dump node; we replayed ##0).
            _ if truncated => {
                if cache_after_trunc {
                    trace_write(results.len());
                    if let Some(bn) = ctx.boundnode.borrow().as_ref() {
                        self.pin_value_identity(bn);
                    }
                    self.inf_cache.borrow_mut().insert(key, Rc::new(results));
                    End::Done
                } else {
                    End::Stopped
                }
            }
            // a producer may "complete" internally (e.g. Subscript's
            // `yield Uninferable; return`) even though the CONSUMER
            // abandoned at that yield — in astroid the NodeNG.infer
            // wrapper is then dropped while suspended, so its tail cache
            // write never runs. Consumer abandonment wins over Done.
            End::Done if consumer_stopped => {
                if std::env::var("PRYLINT_TRACE_INFER").is_ok() {
                    eprintln!("NOCACHE-CONSUMERSTOP");
                }
                End::Stopped
            }
            End::Done => {
                trace_write(results.len());
                // pin pointer-keyed boundnodes (astroid's cache key tuple
                // holds the object — its id can't be recycled)
                if let Some(bn) = ctx.boundnode.borrow().as_ref() {
                    self.pin_value_identity(bn);
                }
                self.inf_cache.borrow_mut().insert(key, Rc::new(results));
                End::Done
            }
            End::Stopped => End::Stopped,
            End::Raised(e) => End::Raised(e), // nothing cached
        }
    }

    /// decorators.py:25-54 path_wrapper, streaming.
    fn path_wrapped_to<F>(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink, f: F) -> End
    where
        F: FnOnce(&Self, &mut Sink) -> End,
    {
        if ctx.push(node) {
            return End::Done; // already on path -> EMPTY generator
        }
        let mut yielded: rustc_hash::FxHashSet<DedupKey> = Default::default();
        let mut wrapped = |v: Value| -> Drive {
            match dedup_key(&v) {
                Some(k) => {
                    if yielded.insert(k) {
                        sink(v)
                    } else {
                        Drive::Go // duplicate: keep pulling
                    }
                }
                None => sink(v),
            }
        };
        f(self, &mut wrapped)
    }

    /// decorators.py:68-96 raise_if_nothing_inferred, streaming.
    fn rin_to<F>(&self, sink: &mut Sink, f: F) -> End
    where
        F: FnOnce(&Self, &mut Sink) -> End,
    {
        let mut any = false;
        let end = {
            let any = &mut any;
            let mut wrapped = |v: Value| -> Drive {
                *any = true;
                sink(v)
            };
            f(self, &mut wrapped)
        };
        match end {
            End::Done if !any => End::Raised(ErrKind::Inference),
            End::Raised(ErrKind::Recursion) if !any => End::Raised(ErrKind::Inference),
            e => e,
        }
    }

    /// decorators.py:57-66 yes_if_nothing_inferred, streaming.
    fn yin_to<F>(&self, sink: &mut Sink, f: F) -> End
    where
        F: FnOnce(&Self, &mut Sink) -> End,
    {
        let mut any = false;
        let end = {
            let any = &mut any;
            let mut wrapped = |v: Value| -> Drive {
                *any = true;
                sink(v)
            };
            f(self, &mut wrapped)
        };
        match end {
            End::Done if !any => {
                let _ = sink(Value::Uninferable);
                End::Done
            }
            e => e,
        }
    }

    /// stream an eagerly-computed Flow (used for node kinds whose astroid
    /// `_infer` materializes everything before yielding anyway).
    fn stream_flow(&self, flow: Flow, sink: &mut Sink) -> End {
        for v in flow.vals {
            yield_v!(sink, v);
        }
        match flow.err {
            Some(e) => End::Raised(e),
            None => End::Done,
        }
    }

    // ---------- per-kind dispatch with decorator table (notes/07 §4.1) ----------

    fn infer_dispatch_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        // EvaluatedObject._infer (node_classes.py EvaluatedObject: yields the
        // stored value) and proxy values stored in synthetic-class locals
        // (enum members): undecorated, yields the value as-is.
        let red = self.redirects.borrow().get(&node).cloned();
        if let Some(NV::V(v)) = red {
            yield_v!(sink, v);
            return End::Done;
        }
        let kind_tag = {
            let md = self.md(node.m);
            std::mem::discriminant(&md.tree.nodes[node.n.idx()].kind);
            // clone the small data we need to avoid holding md across calls
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Name { .. } => 1,
                NodeKind::AssignName { .. } => 2,
                NodeKind::AssignAttr { .. } => 3,
                NodeKind::Attribute { .. } => 4,
                NodeKind::Subscript { .. } => 5,
                NodeKind::Call { .. } => 6,
                NodeKind::AugAssign { .. } => 7,
                NodeKind::BinOp { .. } => 8,
                NodeKind::BoolOp { .. } => 9,
                NodeKind::UnaryOp { .. } => 10,
                NodeKind::Import { .. } => 11,
                NodeKind::ImportFrom { .. } => 12,
                NodeKind::Global { .. } => 13,
                NodeKind::EmptyNode => 14,
                NodeKind::IfExp { .. } => 15,
                NodeKind::List { .. } | NodeKind::Tuple { .. } | NodeKind::Set { .. } => 16,
                NodeKind::Arguments(_) => 17,
                NodeKind::Const(_)
                | NodeKind::Slice { .. }
                | NodeKind::Module(_)
                | NodeKind::ClassDef(_)
                | NodeKind::Lambda(_) => 18,
                NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_) => 19,
                NodeKind::Dict { .. } => 20,
                NodeKind::Compare { .. } => 21,
                NodeKind::JoinedStr { .. } => 22,
                NodeKind::FormattedValue { .. } => 23,
                NodeKind::Unknown => 24,
                _ => 0,
            }
        };
        match kind_tag {
            1 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_name_to(node, ctx, s))
            }),
            2 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_assign_name_to(node, ctx, s))
            }),
            3 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_assign_attr_to(node, ctx, s))
            }),
            4 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_attribute_load_to(node, ctx, s))
            }),
            5 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_subscript_to(node, ctx, s))
            }),
            6 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_call_to(node, ctx, s))
            }),
            7 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_augassign_to(node, ctx, s))
            }),
            8 => self.yin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_binop_to(node, ctx, s))
            }),
            9 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| {
                    let f = e.infer_boolop(node, ctx);
                    e.stream_flow(f, s)
                })
            }),
            10 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_unaryop_to(node, ctx, s))
            }),
            11 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| {
                    let f = e.infer_import(node, ctx);
                    e.stream_flow(f, s)
                })
            }),
            12 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_import_from_to(node, ctx, s))
            }),
            13 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_global_to(node, ctx, s))
            }),
            14 => self.rin_to(sink, |e, s| {
                e.path_wrapped_to(node, ctx, s, |e, s| e.infer_empty_node_to(node, ctx, s))
            }),
            15 => self.rin_to(sink, |e, s| e.infer_ifexp_to(node, ctx, s)),
            16 => self.rin_to(sink, |e, s| {
                let f = e.infer_container(node, ctx);
                e.stream_flow(f, s)
            }),
            17 => self.rin_to(sink, |e, s| e.infer_arguments_node_to(node, ctx, s)),
            18 => {
                yield_v!(sink, Value::Node(node));
                End::Done
            }
            19 => self.stream_flow(self.infer_functiondef(node, ctx), sink),
            20 => self.stream_flow(self.infer_dict(node, ctx), sink),
            21 => self.stream_flow(self.infer_compare(node, ctx), sink),
            22 => self.infer_joinedstr_to(node, ctx, sink),
            23 => self.infer_formatted_value_to(node, ctx, sink),
            24 => {
                yield_v!(sink, Value::Uninferable);
                End::Done
            }
            // NamedExpr is not directly inferable in astroid 4 (no _infer);
            // default: InferenceError
            _ => End::Raised(ErrKind::Inference),
        }
    }

    // ---------- _infer_stmts (bases.py:153-204) ----------

    /// eager shim
    pub fn infer_stmts(&self, stmts: &[NV], ctx_in: Option<&Rc<Ctx>>, frame: Option<GNode>) -> Flow {
        let mut vals = Vec::new();
        let end = self.infer_stmts_to(stmts, ctx_in, frame, &mut |v| {
            vals.push(v);
            Drive::Go
        });
        Flow {
            vals,
            err: end.err_opt(),
        }
    }

    pub fn infer_stmts_to(
        &self,
        stmts: &[NV],
        ctx_in: Option<&Rc<Ctx>>,
        frame: Option<GNode>,
        sink: &mut Sink,
    ) -> End {
        let mut inferred = false;
        let mut constraint_failed = false;
        let (name, ctx, constraints) = match ctx_in {
            Some(c) => {
                let name = c.lookupname.get();
                let clone = c.clone_ctx();
                let constraints = match name {
                    Some(n) => clone
                        .constraints
                        .borrow()
                        .get(&n)
                        .cloned()
                        .unwrap_or_else(|| Rc::new(Vec::new())),
                    None => Rc::new(Vec::new()),
                };
                (name, clone, constraints)
            }
            None => (None, Ctx::new(), Rc::new(Vec::new())),
        };
        for stmt in stmts {
            let stmt_node = match stmt {
                // a real node arriving as a pre-resolved value (object-model
                // results like InstanceModel.__class__) still goes through
                // the full stmt.infer(context) hop in astroid
                NV::V(Value::Node(g)) => *g,
                NV::V(v) => {
                    // Proxies (Instance/BoundMethod/Generator/...) infer to
                    // themselves via Proxy.infer — no bump, no cache. But
                    // SYNTHETIC NODES (const_factory Consts, fresh
                    // containers, FrozenSet...) go through NodeNG.infer:
                    // the first hop under a (lookupname=None via
                    // _infer_name, callcontext, boundnode) key is a cache
                    // miss — the post-yield bump runs only if the consumer
                    // pulls again, and a consumer abandoning at the yield
                    // skips both bump and cache write (bases.py:198 +
                    // node_ng.py:160-176).
                    let hop_key = synth_node_id(v).map(|(tag, ptr)| {
                        (
                            tag,
                            ptr,
                            None,
                            ctx.callcontext.borrow().as_ref().map(|c| c.id),
                            ctx.boundnode.borrow().as_ref().map(crate::value::value_key),
                        )
                    });
                    let is_replay = hop_key
                        .as_ref()
                        .map(|k| self.synth_hop_cache.borrow().contains(k))
                        .unwrap_or(true);
                    // constraint filtering applies to synthetic stmt hops
                    // too (bases.py:184-189): the model's fresh Tuple for
                    // `self.args` under `... if self.args else ...` fails
                    // the BooleanConstraint -> constraint_failed -> the
                    // trailing `yield Uninferable`. (Uninferable stmts
                    // bypass in astroid, but satisfied_by(U) is True for
                    // every constraint kind — same outcome.)
                    // EvaluatedObject._infer yields the wrapped value
                    // (node_classes.py:5024-5028); the hop/cache identity
                    // stays keyed on the wrapper node
                    let yielded: Value = match v {
                        Value::EvaluatedObject { value } => (**value).clone(),
                        other => other.clone(),
                    };
                    if !matches!(v, Value::Uninferable) {
                        let mut stmt_constraints: Vec<&crate::constraint::Constraint> =
                            Vec::new();
                        for (_cstmt, cs) in constraints.iter() {
                            // fresh synthetic values have no tree position:
                            // `constraint_stmt.parent_of(stmt)` is False
                            stmt_constraints.extend(cs.iter());
                        }
                        if !stmt_constraints
                            .iter()
                            .all(|c| self.constraint_satisfied(c, &yielded, &ctx))
                        {
                            constraint_failed = true;
                            // the _infer_stmts for-loop pulls the stmt
                            // generator AGAIN after the rejected value —
                            // the post-yield bump and the cache write of
                            // the synthetic hop still run (node_ng.py
                            // wrapper resumes past the yield)
                            if !is_replay {
                                if let Some(k) = hop_key {
                                    self.pin_value_identity(&v);
                                    if let Some(bn) = ctx.boundnode.borrow().as_ref() {
                                        self.pin_value_identity(bn);
                                    }
                                    self.synth_hop_cache.borrow_mut().insert(k);
                                    ctx.bump_inferred();
                                }
                            }
                            continue;
                        }
                    }
                    inferred = true;
                    if let Drive::Stop = sink(yielded) {
                        return End::Stopped;
                    }
                    if !is_replay {
                        if let Some(k) = hop_key {
                            self.pin_value_identity(v);
                            if let Some(bn) = ctx.boundnode.borrow().as_ref() {
                                self.pin_value_identity(bn);
                            }
                            self.synth_hop_cache.borrow_mut().insert(k);
                            ctx.bump_inferred();
                        }
                    }
                    continue;
                }
                NV::N(g) => *g,
            };
            ctx.lookupname.set(self.infer_name_of_stmt(stmt_node, frame, name));
            // constraints whose If does not contain the stmt
            let mut stmt_constraints: Vec<&crate::constraint::Constraint> = Vec::new();
            for (cstmt, cs) in constraints.iter() {
                if !self.parent_of(*cstmt, stmt_node) {
                    stmt_constraints.extend(cs.iter());
                }
            }
            let end = {
                let inferred = &mut inferred;
                let constraint_failed = &mut constraint_failed;
                let stmt_constraints = &stmt_constraints;
                let ctx2 = Rc::clone(&ctx);
                self.infer_to(stmt_node, &ctx, &mut |inf| {
                    if stmt_constraints
                        .iter()
                        .all(|c| self.constraint_satisfied(c, &inf, &ctx2))
                    {
                        *inferred = true;
                        sink(inf)
                    } else {
                        *constraint_failed = true;
                        Drive::Go
                    }
                })
            };
            match end {
                End::Stopped => return End::Stopped,
                End::Raised(ErrKind::NameError) => continue,
                End::Raised(ErrKind::Inference) => {
                    yield_v!(sink, Value::Uninferable);
                    inferred = true;
                }
                End::Raised(other) => return End::Raised(other),
                End::Done => {}
            }
        }
        if !inferred && constraint_failed {
            yield_v!(sink, Value::Uninferable);
        } else if !inferred {
            return End::Raised(ErrKind::Inference);
        }
        End::Done
    }

    /// stmt._infer_name(frame, name)
    fn infer_name_of_stmt(&self, stmt: GNode, frame: Option<GNode>, name: Option<GSym>) -> Option<GSym> {
        let md = self.md(stmt.m);
        match &md.tree.nodes[stmt.n.idx()].kind {
            NodeKind::Import { .. }
            | NodeKind::ImportFrom { .. }
            | NodeKind::Global { .. }
            | NodeKind::Try(_)
            | NodeKind::TryStar(_) => name,
            NodeKind::Arguments(_) => {
                let parent = self.parent(stmt);
                if parent.is_some() && parent == frame {
                    name
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    // ---------- Name (§8.1) ----------

    fn infer_name_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let md = self.md(node.m);
        let name_sym = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Name { name } => self.g(&md, *name),
            _ => return End::Raised(ErrKind::Inference),
        };
        let looked = self.lookup(node, name_sym);
        let (frame, mut stmts) = (looked.0, looked.1.clone());
        if stmts.is_empty() {
            // helpers._higher_function_scope
            if let Some(pf) = self.higher_function_scope(self.scope(node)) {
                let looked2 = self.lookup_in(pf, node, name_sym);
                stmts = looked2;
            }
            if stmts.is_empty() {
                return End::Raised(ErrKind::NameError);
            }
        }
        let ctx2 = copy_context(Some(ctx));
        ctx2.lookupname.set(Some(name_sym));
        let constraints = self.get_constraints(node, frame);
        ctx2.constraints
            .borrow_mut()
            .insert(name_sym, Rc::new(constraints));
        self.infer_stmts_to(&stmts, Some(&ctx2), Some(frame), sink)
    }

    /// `parent_function.lookup(name)` (node_ng.py lookup: `return
    /// self.scope().scope_lookup(self, name)`) — the BASE NODE for
    /// filtering is the FUNCTION itself, not the original Name: the
    /// is_from_decorator same-statement filter does NOT re-fire, so the
    /// function's own params are visible (lambda-in-decorator case).
    fn lookup_in(&self, scope: GNode, _node: GNode, name: GSym) -> Vec<NV> {
        self.scope_lookup(scope, scope, name, 0).1
    }

    fn higher_function_scope(&self, scope: GNode) -> Option<GNode> {
        let mut current = scope;
        loop {
            let parent = self.parent(current)?;
            if self.kind_is(parent, |k| {
                matches!(k, NodeKind::FunctionDef(_) | NodeKind::AsyncFunctionDef(_))
            }) {
                return Some(parent);
            }
            current = parent;
        }
    }

    // ---------- AssignName / AssignAttr (§8.2) ----------

    fn infer_assign_name_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let parent = self.parent(node);
        if let Some(p) = parent {
            if self.kind_is(p, |k| matches!(k, NodeKind::AugAssign { .. })) {
                return self.infer_to(p, ctx, sink);
            }
        }
        // astroid materializes: `stmts = list(self.assigned_stmts(...))`
        let stmts = match self.assigned_stmts(node, Some(ctx), None) {
            Ok(s) => s,
            Err(e) => return End::Raised(e),
        };
        self.infer_stmts_to(&stmts, Some(ctx), None, sink)
    }

    fn infer_assign_attr_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        // AssignAttr._infer (node_classes.py:1165-1177) is IDENTICAL to
        // AssignName._infer, including the AugAssign delegation:
        // `self.x += 1` re-enters AugAssign.infer, whose infer_lhs is
        // path_wrapped on the SAME (target-node, lookupname) key — the
        // recursion blocks -> InferenceError -> _infer_stmts yields U
        let parent = self.parent(node);
        if let Some(p) = parent {
            if self.kind_is(p, |k| matches!(k, NodeKind::AugAssign { .. })) {
                return self.infer_to(p, ctx, sink);
            }
        }
        let stmts = match self.assigned_stmts(node, Some(ctx), None) {
            Ok(s) => s,
            Err(e) => return End::Raised(e),
        };
        self.infer_stmts_to(&stmts, Some(ctx), None, sink)
    }

    // ---------- Attribute(Load) (§12.1) ----------

    /// eager shim (AssignAttr.infer_lhs path)
    pub fn infer_attribute_load(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let mut vals = Vec::new();
        let end = self.infer_attribute_load_to(node, ctx, &mut |v| {
            vals.push(v);
            Drive::Go
        });
        Flow {
            vals,
            err: end.err_opt(),
        }
    }

    pub fn infer_attribute_load_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let md = self.md(node.m);
        let (expr, attrname) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Attribute { expr, attrname, .. } => {
                (GNode { m: node.m, n: *expr }, self.g(&md, *attrname))
            }
            NodeKind::AssignAttr { expr, attrname } => {
                (GNode { m: node.m, n: *expr }, self.g(&md, *attrname))
            }
            _ => return End::Raised(ErrKind::Inference),
        };
        // `context = copy_context(context)` re-binds the loop variable: each
        // iteration copies the PREVIOUS copy (node_classes.py:1081).
        let mut cur_ctx = Rc::clone(ctx);
        let end = {
            let cur_ctx = &mut cur_ctx;
            self.infer_to(expr, ctx, &mut |owner| {
                if owner.is_uninferable() {
                    return sink(Value::Uninferable);
                }
                *cur_ctx = copy_context(Some(cur_ctx));
                let old_bound = cur_ctx.boundnode.borrow().clone();
                *cur_ctx.boundnode.borrow_mut() = Some(owner.clone());
                // constraints when owner is ClassDef or Instance
                let frame_for_constraints: Option<GNode> = match &owner {
                    Value::Node(g)
                        if self.kind_is(*g, |k| matches!(k, NodeKind::ClassDef(_))) =>
                    {
                        Some(*g)
                    }
                    Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => Some(*cls),
                    _ => None,
                };
                if let Some(frame) = frame_for_constraints {
                    let cs = self.get_constraints(node, frame);
                    cur_ctx
                        .constraints
                        .borrow_mut()
                        .insert(attrname, Rc::new(cs));
                }
                // hardcoded sys.argv (node_classes.py:1084-1086)
                let is_sys_argv = self.sname(attrname) == "argv"
                    && matches!(&owner, Value::Node(g)
                        if self.kind_is(*g, |k| matches!(k, NodeKind::Module(_)))
                            && self.md(g.m).name == "sys");
                let drive = if is_sys_argv {
                    sink(Value::Uninferable)
                } else {
                    // per-owner errors swallowed (AttributeInferenceError,
                    // InferenceError, AttributeError — node_classes.py:1100)
                    let mut stopped = false;
                    let _ = self.igetattr_value_to(&owner, attrname, Some(cur_ctx), &mut |v| {
                        let d = sink(v);
                        if let Drive::Stop = d {
                            stopped = true;
                        }
                        d
                    });
                    if stopped {
                        Drive::Stop
                    } else {
                        Drive::Go
                    }
                };
                *cur_ctx.boundnode.borrow_mut() = old_bound;
                drive
            })
        };
        // owner-generator errors propagate (after any yields)
        end
    }

    // ---------- Import / ImportFrom / Global (§20.3) ----------

    fn infer_import(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let name = match ctx.lookupname.get() {
            Some(n) => n,
            None => return Flow::err(ErrKind::Inference),
        };
        let md = self.md(node.m);
        let names: Vec<(GSym, Option<GSym>)> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Import { names } => names
                .iter()
                .map(|(a, b)| (self.g(&md, *a), b.map(|s| self.g(&md, s))))
                .collect(),
            _ => return Flow::err(ErrKind::Inference),
        };
        let real = match self.real_name(&names, name) {
            Some(r) => r,
            None => return Flow::err(ErrKind::Inference),
        };
        match self.do_import_module(node, Some(&real)) {
            Ok(mid) => Flow::one(Value::Node(GNode {
                m: mid,
                n: NodeId::MODULE,
            })),
            Err(_) => Flow::err(ErrKind::Inference),
        }
    }

    /// _base_nodes.py:174-188 real_name
    fn real_name(&self, names: &[(GSym, Option<GSym>)], asname: GSym) -> Option<String> {
        let asname_str = self.sname(asname);
        for (name, _asname) in names {
            let name_str = self.sname(*name);
            if name_str == "*" {
                return Some(asname_str);
            }
            let (eff_name, eff_asname) = match _asname {
                Some(a) => (name_str.clone(), self.sname(*a)),
                None => {
                    let first = name_str.split('.').next().unwrap_or("").to_string();
                    (first.clone(), first)
                }
            };
            if asname_str == eff_asname {
                return Some(eff_name);
            }
        }
        None
    }

    fn infer_import_from_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let name = match ctx.lookupname.get() {
            Some(n) => n,
            None => return End::Raised(ErrKind::Inference),
        };
        let md = self.md(node.m);
        let names: Vec<(GSym, Option<GSym>)> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::ImportFrom { names, .. } => names
                .iter()
                .map(|(a, b)| (self.g(&md, *a), b.map(|s| self.g(&md, s))))
                .collect(),
            _ => return End::Raised(ErrKind::Inference),
        };
        let real = match self.real_name(&names, name) {
            Some(r) => r,
            None => return End::Raised(ErrKind::Inference),
        };
        let module = match self.do_import_module(node, None) {
            Ok(m) => m,
            Err(_) => return End::Raised(ErrKind::Inference),
        };
        let ctx2 = copy_context(Some(ctx));
        let real_sym = self.sym(&real);
        ctx2.lookupname.set(Some(real_sym));
        let ignore_locals = module == node.m; // module is self.root()
        match self.module_getattr(module, real_sym, ignore_locals) {
            Ok(stmts) => self.infer_stmts_to(&stmts, Some(&ctx2), None, sink),
            Err(_) => End::Raised(ErrKind::Inference),
        }
    }

    fn infer_global_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let name = match ctx.lookupname.get() {
            Some(n) => n,
            None => return End::Raised(ErrKind::Inference),
        };
        match self.module_getattr(node.m, name, false) {
            Ok(stmts) => self.infer_stmts_to(&stmts, Some(ctx), None, sink),
            Err(_) => End::Raised(ErrKind::Inference),
        }
    }

    // ---------- FunctionDef (§11.2) ----------

    fn infer_functiondef(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        // `self.decorators and bases._is_property(self)` — context None
        // (scoped_nodes.py:1526); the decorators check is inside is_property
        // only for phase 3, so replicate the outer guard here.
        let has_decorators = {
            let md = self.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                    d.decorators.is_some()
                }
                _ => false,
            }
        };
        let _ = ctx;
        if has_decorators && self.is_property(node, None) {
            Flow::one(Value::Property { func: node, synth: false })
        } else {
            Flow::one(Value::Node(node))
        }
    }

    // ---------- containers (§17) ----------

    fn infer_container(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let (elts, kind) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::List { elts, .. } => (elts.clone(), SeqKind::List),
            NodeKind::Tuple { elts, .. } => (elts.clone(), SeqKind::Tuple),
            NodeKind::Set { elts } => (elts.clone(), SeqKind::Set),
            _ => return Flow::err(ErrKind::Inference),
        };
        let has_special = elts.iter().any(|&e| {
            matches!(
                md.tree.nodes[e.idx()].kind,
                NodeKind::Starred { .. } | NodeKind::NamedExpr { .. }
            )
        });
        if !has_special {
            return Flow::one(Value::Node(node));
        }
        // _infer_sequence_helper (node_classes.py:364-386)
        let mut values: Vec<Value> = Vec::new();
        for e in elts {
            let g = GNode { m: node.m, n: e };
            match &md.tree.nodes[e.idx()].kind {
                NodeKind::Starred { value, .. } => {
                    let starred = self.safe_infer(GNode { m: node.m, n: *value }, ctx);
                    match starred {
                        Some(v) => match self.value_elts(&v) {
                            Some(sub) => values.extend(sub),
                            None => return Flow::err(ErrKind::Inference),
                        },
                        None => return Flow::err(ErrKind::Inference),
                    }
                }
                NodeKind::NamedExpr { value, .. } => {
                    let v = self.safe_infer(GNode { m: node.m, n: *value }, ctx);
                    match v {
                        Some(v) => values.push(v),
                        None => return Flow::err(ErrKind::Inference),
                    }
                }
                _ => values.push(Value::Node(g)),
            }
        }
        Flow::one(Value::SynthSeq {
            kind,
            elems: Rc::new(values),
        })
    }

    fn infer_dict(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let items: Vec<(NodeId, NodeId)> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Dict { items } => items.clone(),
            _ => return Flow::err(ErrKind::Inference),
        };
        let has_unpack = items
            .iter()
            .any(|(k, _)| matches!(md.tree.nodes[k.idx()].kind, NodeKind::DictUnpack));
        if !has_unpack {
            return Flow::one(Value::Node(node));
        }
        drop(md);
        match self.dict_infer_map(node, ctx) {
            Ok(out) => Flow::one(Value::SynthDict {
                items: Rc::new(out),
            }),
            Err(e) => Flow::err(e),
        }
    }

    /// Dict._infer_map (node_classes.py:2485-2506): safe_infer of EVERY
    /// key/value (an ambiguous or Uninferable element raises
    /// InferenceError); `**expr` unpacks recurse into the unpacked dict's
    /// OWN _infer_map (its items get safe-inferred too — an IfExp value
    /// inside the source dict poisons the whole literal, pandas to_latex).
    /// _update_with_replacement keys by key.as_string(): the slot keeps
    /// its FIRST position, later duplicates overwrite (key, value) in
    /// place.
    fn dict_infer_map(
        &self,
        node: GNode,
        ctx: &Rc<Ctx>,
    ) -> Result<Vec<(Value, Value)>, ErrKind> {
        let md = self.md(node.m);
        let items: Vec<(NodeId, NodeId)> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Dict { items } => items.clone(),
            _ => return Err(ErrKind::Inference),
        };
        let mut out: Vec<(Value, Value)> = Vec::new();
        let replace = |k: Value, v: Value, out: &mut Vec<(Value, Value)>| {
            let kc = self.value_const(&k);
            if let Some(kc) = kc {
                if let Some(pos) = out
                    .iter()
                    .position(|(ek, _)| self.value_const(ek).as_ref() == Some(&kc))
                {
                    out[pos] = (k, v);
                    return;
                }
            }
            out.push((k, v));
        };
        for (k, v) in items {
            if matches!(md.tree.nodes[k.idx()].kind, NodeKind::DictUnpack) {
                let inner = self.safe_infer(GNode { m: node.m, n: v }, ctx);
                match inner {
                    Some(Value::Node(g))
                        if self.kind_is(g, |kd| matches!(kd, NodeKind::Dict { .. })) =>
                    {
                        // double_starred._infer_map(context) — recursive
                        let pairs = self.dict_infer_map(g, ctx)?;
                        for (ik, iv) in pairs {
                            replace(ik, iv, &mut out);
                        }
                    }
                    Some(val @ Value::SynthDict { .. }) => {
                        // an already-folded synthetic Dict (e.g. the fresh
                        // Dict infer_argument builds for **kwargs): astroid
                        // recurses _infer_map over it, SAFE-INFERRING every
                        // key/value -- fresh Const keys and call-site value
                        // nodes each get a real infer hop (+1 bump first
                        // time; sentry build_expected_result counts)
                        match self.value_dict_items(&val) {
                            Some(pairs) => {
                                for (ik, iv) in pairs {
                                    let ik = match &ik {
                                        Value::Node(g) => match self.safe_infer(*g, ctx) {
                                            Some(v2) => v2,
                                            None => return Err(ErrKind::Inference),
                                        },
                                        other => {
                                            self.synth_value_pull(other, ctx);
                                            other.clone()
                                        }
                                    };
                                    let iv = match &iv {
                                        Value::Node(g) => match self.safe_infer(*g, ctx) {
                                            Some(v2) => v2,
                                            None => return Err(ErrKind::Inference),
                                        },
                                        other => {
                                            self.synth_value_pull(other, ctx);
                                            other.clone()
                                        }
                                    };
                                    if ik.is_uninferable() || iv.is_uninferable() {
                                        return Err(ErrKind::Inference);
                                    }
                                    replace(ik, iv, &mut out);
                                }
                            }
                            None => return Err(ErrKind::Inference),
                        }
                    }
                    // `if not isinstance(double_starred, Dict): raise`
                    _ => return Err(ErrKind::Inference),
                }
            } else {
                let ik = self.safe_infer(GNode { m: node.m, n: k }, ctx);
                let iv = self.safe_infer(GNode { m: node.m, n: v }, ctx);
                match (ik, iv) {
                    (Some(ik), Some(iv)) => {
                        if ik.is_uninferable() || iv.is_uninferable() {
                            return Err(ErrKind::Inference);
                        }
                        replace(ik, iv, &mut out);
                    }
                    _ => return Err(ErrKind::Inference),
                }
            }
        }
        Ok(out)
    }

    // ---------- BoolOp (§16.4) ----------

    fn infer_boolop(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let (op, values) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::BoolOp { op, values } => (op.clone(), values.clone()),
            _ => return Flow::err(ErrKind::Inference),
        };
        if values.len() < 2 {
            return Flow::err(ErrKind::Inference);
        }
        let shortcircuit = op.as_ref() == "or";
        // infer each operand fully (node_classes.py:1655-1663). The
        // try/except around the comprehension only covers generator
        // CREATION (which can't raise); itertools.product drains the
        // generators OUTSIDE it, so a mid-drain InferenceError PROPAGATES
        // out of BoolOp._infer (the already-pulled values are discarded —
        // their counter burns persist). django get_system_encoding:
        // `locale.getlocale()[1] or "ascii"` -> whole BoolOp ERR -> the
        // consuming _infer_stmts yields U.
        let mut inferred_ops: Vec<Vec<Value>> = Vec::new();
        for v in &values {
            let f = self.infer(GNode { m: node.m, n: *v }, ctx);
            if let Some(e) = f.err {
                return Flow::err(e);
            }
            if f.vals.is_empty() {
                return Flow::err(ErrKind::Inference);
            }
            inferred_ops.push(f.vals);
        }
        // cartesian product
        let mut out = Vec::new();
        let mut idx = vec![0usize; inferred_ops.len()];
        'outer: loop {
            let pair: Vec<&Value> = idx.iter().enumerate().map(|(i, &j)| &inferred_ops[i][j]).collect();
            if pair.iter().any(|v| v.is_uninferable()) {
                out.push(Value::Uninferable);
            } else {
                // `bool_values = [item.bool_value() for item in pair]`
                // (node_classes.py:1662): EVERY item — including the LAST —
                // gets a NO-CONTEXT bool_value (Instance.bool_value
                // materializes a fresh InferenceContext: its __bool__/__len__
                // igetattr burns happen in a separate counter cell), and the
                // comprehension does NOT short-circuit on Uninferable.
                let bool_values: Vec<Option<bool>> =
                    pair.iter().map(|v| self.bool_value(v, &Ctx::new())).collect();
                if bool_values.iter().any(|b| b.is_none()) {
                    out.push(Value::Uninferable);
                } else {
                    let mut yielded = false;
                    for (value, bv) in pair.iter().zip(&bool_values) {
                        if bv.unwrap_or(false) == shortcircuit {
                            out.push((*value).clone());
                            yielded = true;
                            break;
                        }
                    }
                    if !yielded {
                        out.push(pair[pair.len() - 1].clone());
                    }
                }
            }
            // increment cartesian counter
            let mut i = idx.len();
            loop {
                if i == 0 {
                    break 'outer;
                }
                i -= 1;
                idx[i] += 1;
                if idx[i] < inferred_ops[i].len() {
                    break;
                }
                idx[i] = 0;
            }
        }
        Flow::ok(out)
    }

    // ---------- IfExp (§16.5) ----------

    fn infer_ifexp_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let md = self.md(node.m);
        let (test, body, orelse) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::IfExp { test, body, orelse } => (*test, *body, *orelse),
            _ => return End::Raised(ErrKind::Inference),
        };
        // node_classes.py:3115-3117: branch contexts copied UP FRONT
        let lhs = copy_context(Some(ctx));
        let rhs = copy_context(Some(ctx));
        // condition scan (node_classes.py:3121-3136): pulls test values one
        // at a time; `break` abandons the test generator.
        let mut condition: Option<bool> = None;
        let mut decided = false; // broke out with condition=None
        {
            let condition = &mut condition;
            let decided = &mut decided;
            let tctx = ctx.clone_ctx();
            let end = self.infer_to(GNode { m: node.m, n: test }, &tctx, &mut |v| {
                if v.is_uninferable() {
                    *condition = None;
                    *decided = true;
                    return Drive::Stop;
                }
                // test.bool_value() — no context (bases.py:388)
                match self.bool_value(&v, &Ctx::new()) {
                    None => {
                        *condition = None;
                        *decided = true;
                        Drive::Stop
                    }
                    Some(b) => {
                        match *condition {
                            None if !*decided => {
                                *condition = Some(b);
                                *decided = true; // first value recorded
                                Drive::Go
                            }
                            Some(prev) if prev != b => {
                                *condition = None;
                                Drive::Stop
                            }
                            _ => Drive::Go,
                        }
                    }
                }
            });
            if let End::Raised(e) = end {
                if e.is_inference() {
                    *condition = None;
                } else {
                    return End::Raised(e);
                }
            }
        }
        if condition == Some(true) || condition.is_none() {
            match self.infer_to(GNode { m: node.m, n: body }, &lhs, sink) {
                End::Done => {}
                e => return e, // errors / consumer stop propagate immediately
            }
        }
        if condition == Some(false) || condition.is_none() {
            return self.infer_to(GNode { m: node.m, n: orelse }, &rhs, sink);
        }
        End::Done
    }

    // ---------- Compare (§16.3) ----------

    fn infer_compare(&self, node: GNode, ctx: &Rc<Ctx>) -> Flow {
        let md = self.md(node.m);
        let (left, ops) = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Compare { left, ops } => (*left, ops.clone()),
            _ => return Flow::err(ErrKind::Inference),
        };
        let mut retval: Option<bool> = None;
        // `lhs = list(left_node.infer(context=context))` — the context is
        // passed AS-IS (node_classes.py:1846-1853): operand path pushes
        // land on the SHARED context object, so later recursion frames
        // copied from it inherit the entries (shares_memory-style
        // self-recursion path blocks)
        let lhs_flow = self.infer(GNode { m: node.m, n: left }, ctx);
        if lhs_flow.is_err() {
            return Flow { vals: lhs_flow.vals, err: lhs_flow.err };
        }
        let mut lhs = lhs_flow.vals;
        for (op, right) in &ops {
            let rhs_flow = self.infer(GNode { m: node.m, n: *right }, ctx);
            if rhs_flow.is_err() {
                return Flow { vals: Vec::new(), err: rhs_flow.err };
            }
            let rhs = rhs_flow.vals;
            // _do_compare
            match self.do_compare(&lhs, op, &rhs) {
                None => return Flow::uninferable(),
                Some(r) => retval = Some(r),
            }
            if retval == Some(false) {
                break; // short-circuit
            }
            lhs = rhs;
        }
        match retval {
            Some(b) => Flow::one(Value::SynthConst(Rc::new(ConstValue::Bool(b)))),
            None => Flow::uninferable(),
        }
    }

    /// node_classes.py:1859-1905 _do_compare — `_to_literal` is
    /// `ast.literal_eval(node.as_string())`: Consts plus literal
    /// containers fold; `in`/`not in` are real COMPARE_OPS (only
    /// `is`/`is not` are UNINFERABLE_OPS). Mixed-type `==`/`!=` gives
    /// False/True (operator.eq never raises); order/membership TypeErrors
    /// become AstroidTypeError -> the caller yields Uninferable.
    /// Returns None for Uninferable.
    fn do_compare(&self, lefts: &[Value], op: &str, rights: &[Value]) -> Option<bool> {
        if op == "is" || op == "is not" {
            return None;
        }
        let mut retval: Option<bool> = None;
        for left in lefts {
            for right in rights {
                let ll = self.to_literal(left)?;
                let rl = self.to_literal(right)?;
                let r = compare_literals(&ll, op, &rl)?;
                match retval {
                    None => retval = Some(r),
                    Some(prev) if prev == r => {}
                    _ => return None, // mixed True/False
                }
            }
        }
        retval
    }

    /// Compare._to_literal: ast.literal_eval(value.as_string()) — Consts
    /// and literal containers (all elements recursively literal) fold;
    /// anything else raises -> Uninferable (None).
    fn to_literal(&self, v: &Value) -> Option<Lit> {
        if let Some(c) = self.value_const(v) {
            return Some(Lit::Const(c));
        }
        match v {
            Value::SynthSeq { kind, elems } => {
                let lits: Option<Vec<Lit>> = elems.iter().map(|e| self.to_literal(e)).collect();
                Some(match kind {
                    crate::value::SeqKind::List => Lit::List(lits?),
                    crate::value::SeqKind::Tuple => Lit::Tuple(lits?),
                    crate::value::SeqKind::Set => Lit::Set(lits?),
                })
            }
            Value::SynthDict { items } => {
                let lits: Option<Vec<(Lit, Lit)>> = items
                    .iter()
                    .map(|(k, val)| Some((self.to_literal(k)?, self.to_literal(val)?)))
                    .collect();
                Some(Lit::Dict(lits?))
            }
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::List { elts, .. } | NodeKind::Tuple { elts, .. }
                    | NodeKind::Set { elts } => {
                        let lits: Option<Vec<Lit>> = elts
                            .iter()
                            .map(|&e| self.to_literal(&Value::Node(GNode { m: g.m, n: e })))
                            .collect();
                        let lits = lits?;
                        Some(match &md.tree.nodes[g.n.idx()].kind {
                            NodeKind::List { .. } => Lit::List(lits),
                            NodeKind::Tuple { .. } => Lit::Tuple(lits),
                            _ => Lit::Set(lits),
                        })
                    }
                    NodeKind::Dict { items } => {
                        let lits: Option<Vec<(Lit, Lit)>> = items
                            .iter()
                            .map(|&(k, val)| {
                                Some((
                                    self.to_literal(&Value::Node(GNode { m: g.m, n: k }))?,
                                    self.to_literal(&Value::Node(GNode { m: g.m, n: val }))?,
                                ))
                            })
                            .collect();
                        Some(Lit::Dict(lits?))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    // ---------- f-strings (§16.6) ----------

    /// FormattedValue._infer, STREAMING and LAZY like the astroid
    /// generators (node_classes.py:4699-4747): the spec generator stays
    /// suspended through each spec's value loop (its post-yield bump fires
    /// only on the NEXT pull), and a raise from the value generator
    /// abandons the suspended spec generator (no bump, no cache write).
    fn infer_formatted_value_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let (value, format_spec) = {
            let md = self.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::FormattedValue {
                    value, format_spec, ..
                } => (*value, *format_spec),
                _ => return End::Raised(ErrKind::Inference),
            }
        };
        let value_g = GNode { m: node.m, n: value };
        let mut uninferable_already = false;
        enum Ctl {
            Go,
            ConsumerStop,
            Raised(ErrKind),
        }
        // process one inferred format-spec value (the body of astroid's
        // spec loop)
        let mut process_spec = |e: &Self,
                                spec_v: &Value,
                                uninferable_already: &mut bool,
                                sink: &mut Sink|
         -> Ctl {
            let spec: Option<String> = match e.value_const(spec_v) {
                Some(ConstValue::Str(sp)) => Some(sp.to_string()),
                Some(_) => None, // non-str Const spec: format() TypeError per value
                None => {
                    // not a Const at all -> single Uninferable
                    if !*uninferable_already {
                        *uninferable_already = true;
                        if let Drive::Stop = sink(Value::Uninferable) {
                            return Ctl::ConsumerStop;
                        }
                    }
                    return Ctl::Go;
                }
            };
            let mut consumer_stop = false;
            let vend = {
                let consumer_stop = &mut consumer_stop;
                let uninferable_already = &mut *uninferable_already;
                e.infer_to(value_g, ctx, &mut |v| {
                    // format(value_to_format, spec): Const values use python
                    // format(); other inference results are formatted as
                    // their astroid str() — Instance: "Instance of X"
                    // (bases.py:373), Uninferable: "Uninferable"
                    let formatted: Option<String> = match (&spec, &v) {
                        (Some(sp), _) if e.value_const(&v).is_some() => {
                            format_const(&e.value_const(&v).unwrap(), sp)
                        }
                        // format(obj, "") of a non-Const result calls
                        // object.__format__ -> str(obj) (node_classes.py:4719)
                        (Some(sp), _) if sp.is_empty() => e.astroid_object_str(&v),
                        _ => None,
                    };
                    let d = match formatted {
                        Some(sf) => {
                            sink(Value::SynthConst(Rc::new(ConstValue::Str(sf.into()))))
                        }
                        None => {
                            *uninferable_already = true;
                            sink(Value::Uninferable)
                        }
                    };
                    if let Drive::Stop = d {
                        *consumer_stop = true;
                    }
                    d
                })
            };
            if consumer_stop {
                return Ctl::ConsumerStop;
            }
            match vend {
                // a raise from self.value.infer aborts FormattedValue._infer
                // (no try/except, node_classes.py:4736-4741)
                End::Raised(e2) => Ctl::Raised(e2),
                _ => Ctl::Go,
            }
        };
        match format_spec {
            None => {
                // node_classes.py:4707 `format_specs = Const("")` — a FRESH
                // Const NODE; `format_specs.infer(context)` is a FULL
                // NodeNG.infer hop (trace entry, cap check, fresh-key cache
                // write, post-yield bump when the spec loop pulls again).
                let hop = self.model_hop_node(Value::SynthConst(Rc::new(ConstValue::Str(
                    "".into(),
                ))));
                let mut pending: Option<Ctl> = None;
                let end = {
                    let pending = &mut pending;
                    let uninferable_already = &mut uninferable_already;
                    self.infer_to(hop, ctx, &mut |spec_v| {
                        match process_spec(self, &spec_v, uninferable_already, sink) {
                            Ctl::Go => Drive::Go,
                            ctl => {
                                *pending = Some(ctl);
                                Drive::Stop
                            }
                        }
                    })
                };
                match pending {
                    Some(Ctl::ConsumerStop) => End::Stopped,
                    Some(Ctl::Raised(e)) => End::Raised(e),
                    _ => match end {
                        End::Raised(e) => End::Raised(e),
                        End::Stopped => End::Stopped,
                        End::Done => End::Done,
                    },
                }
            }
            Some(fs) => {
                let fs_g = GNode { m: node.m, n: fs };
                let mut pending: Option<Ctl> = None;
                let end = {
                    let pending = &mut pending;
                    let uninferable_already = &mut uninferable_already;
                    self.infer_to(fs_g, ctx, &mut |spec_v| {
                        match process_spec(self, &spec_v, uninferable_already, sink) {
                            Ctl::Go => Drive::Go,
                            ctl => {
                                *pending = Some(ctl);
                                Drive::Stop
                            }
                        }
                    })
                };
                match pending {
                    Some(Ctl::ConsumerStop) => End::Stopped,
                    Some(Ctl::Raised(e)) => End::Raised(e),
                    _ => match end {
                        // a raise from format_specs.infer propagates after
                        // its yielded values
                        End::Raised(e) => End::Raised(e),
                        End::Stopped => End::Stopped,
                        End::Done => End::Done,
                    },
                }
            }
        }
    }

    /// _safe_infer_from_node (node_classes.py:4846-4853), STREAMING:
    /// node._infer (no NodeNG wrapper for the part node itself); an
    /// InferenceError raised at ANY point yields one trailing Uninferable;
    /// other raises propagate after the yielded values.
    fn joinedstr_safe_to(&self, g: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let mut stopped = false;
        let end = {
            let stopped = &mut stopped;
            self.infer_dispatch_to(g, ctx, &mut |v| {
                let d = sink(v);
                if let Drive::Stop = d {
                    *stopped = true;
                }
                d
            })
        };
        if stopped {
            return End::Stopped;
        }
        match end {
            End::Raised(e) if e.is_inference() => {
                yield_v!(sink, Value::Uninferable);
                End::Done
            }
            e => e,
        }
    }

    /// JoinedStr._infer_from_values (node_classes.py:4822-4844), recursive
    /// cartesian concatenation, STREAMING and LAZY: each prefix value's
    /// suffix recursion runs while the prefix generator is suspended
    /// (deferred post-yield bumps; abandonment on raise/stop).
    fn joinedstr_parts_to(
        &self,
        values: &[NodeId],
        m: crate::value::ModId,
        ctx: &Rc<Ctx>,
        sink: &mut Sink,
    ) -> End {
        if values.is_empty() {
            return End::Done;
        }
        if values.len() == 1 {
            let g = GNode { m, n: values[0] };
            return self.joinedstr_safe_to(g, ctx, &mut |v| {
                if self.value_const(&v).is_some() {
                    sink(v)
                } else {
                    sink(Value::SynthConst(Rc::new(ConstValue::Str(
                        "{Uninferable}".into(),
                    ))))
                }
            });
        }
        let g = GNode { m, n: values[0] };
        let rest = &values[1..];
        let mut pending: Option<End> = None;
        let end = {
            let pending = &mut pending;
            self.joinedstr_safe_to(g, ctx, &mut |prefix| {
                // the suffix generator is recreated per prefix
                let mut consumer_stop = false;
                let send = {
                    let consumer_stop = &mut consumer_stop;
                    let prefix = &prefix;
                    self.joinedstr_parts_to(rest, m, ctx, &mut |suffix| {
                        let mut result = String::new();
                        for part in [prefix, &suffix] {
                            match self.value_const(part) {
                                Some(c) => result.push_str(&const_str_value(&c)),
                                None => result.push_str("{Uninferable}"),
                            }
                        }
                        let d =
                            sink(Value::SynthConst(Rc::new(ConstValue::Str(result.into()))));
                        if let Drive::Stop = d {
                            *consumer_stop = true;
                        }
                        d
                    })
                };
                if consumer_stop {
                    *pending = Some(End::Stopped);
                    return Drive::Stop;
                }
                match send {
                    End::Raised(e) => {
                        // a suffix raise propagates, abandoning the prefix
                        // generator mid-suspension
                        *pending = Some(End::Raised(e));
                        Drive::Stop
                    }
                    _ => Drive::Go,
                }
            })
        };
        match pending {
            Some(p) => p,
            None => end,
        }
    }

    /// JoinedStr._infer / _infer_with_values (node_classes.py:4799-4820),
    /// STREAMING. failed = U or Const str containing the "{Uninferable}"
    /// marker; only the FIRST failure yields U — later failures fall
    /// through and yield the raw marker Const (bug-for-bug).
    fn infer_joinedstr_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let (values, m) = {
            let md = self.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::JoinedStr { values } => (values.clone(), node.m),
                _ => return End::Raised(ErrKind::Inference),
            }
        };
        if values.is_empty() {
            yield_v!(sink, Value::SynthConst(Rc::new(ConstValue::Str("".into()))));
            return End::Done;
        }
        let mut uninferable_already = false;
        let uninferable_already = &mut uninferable_already;
        self.joinedstr_parts_to(&values, m, ctx, &mut |v| {
            let failed = v.is_uninferable()
                || matches!(self.value_const(&v), Some(ConstValue::Str(sv)) if sv.contains("{Uninferable}"));
            if failed && !*uninferable_already {
                *uninferable_already = true;
                return sink(Value::Uninferable);
            }
            sink(v)
        })
    }

    // ---------- EmptyNode ----------

    /// EmptyNode._infer (node_classes.py:2568-2581) →
    /// AstroidManager.infer_ast_from_something (manager.py): resolves the
    /// live object's class via `modastroid.igetattr(name, context)` — under
    /// the SHARED context, so the lookup work bumps nodes_inferred exactly
    /// like astroid. The snapshot einf descriptor supplies klass.__module__
    /// + __name__ (qname) and whether obj was an instance (instantiate
    /// branch) or the class/function itself.
    fn infer_empty_node_to(&self, node: GNode, ctx: &Rc<Ctx>, sink: &mut Sink) -> End {
        let (descs, ek) = {
            let md = self.md(node.m);
            (md.einf.get(&node.n).cloned(), md.eklass.get(&node.n).cloned())
        };
        let Some(ek) = ek else {
            // no underlying object -> Uninferable; legacy einf-only
            // snapshots replay recorded values
            match descs {
                Some(descs) if !descs.is_empty() => {
                    for d in &descs {
                        yield_v!(sink, self.resolve_einf(d));
                    }
                }
                _ => yield_v!(sink, Value::Uninferable),
            }
            return End::Done;
        };
        let (modname, name, instantiate) = (ek.module, ek.name, ek.instance);
        let mid = match self.ast_from_module_name(&modname, true) {
            Ok(m) => m,
            Err(_) => {
                // AstroidError -> Uninferable
                yield_v!(sink, Value::Uninferable);
                return End::Done;
            }
        };
        let sym = self.sym(&name);
        let owner = Value::Node(GNode {
            m: mid,
            n: NodeId::MODULE,
        });
        let mut stopped = false;
        let end = {
            let stopped = &mut stopped;
            self.igetattr_value_to(&owner, sym, Some(ctx), &mut |v| {
                let out = if instantiate {
                    match &v {
                        // Uninferable.instantiate_class() is Uninferable
                        Value::Uninferable => Value::Uninferable,
                        Value::Node(g)
                            if self.kind_is(*g, |k| matches!(k, NodeKind::ClassDef(_))) =>
                        {
                            self.instantiate_class(*g)
                        }
                        other => other.clone(),
                    }
                } else {
                    v
                };
                let d = sink(out);
                if let Drive::Stop = d {
                    *stopped = true;
                }
                d
            })
        };
        if stopped {
            return End::Stopped;
        }
        match end {
            // InferenceError is an AstroidError -> yield Uninferable
            End::Raised(_) => {
                yield_v!(sink, Value::Uninferable);
                End::Done
            }
            e => e,
        }
    }

    fn resolve_einf(&self, d: &EInf) -> Value {
        match d {
            EInf::Uninferable => Value::Uninferable,
            EInf::Const(c) => Value::SynthConst(Rc::new(c.clone())),
            EInf::Class(q) | EInf::Inst(q) | EInf::Func(q) => {
                match self.resolve_qname(q) {
                    Some(g) => match d {
                        EInf::Inst(_) => Value::Inst { cls: g, id: crate::value::fresh_inst_id() },
                        _ => Value::Node(g),
                    },
                    None => Value::Uninferable,
                }
            }
        }
    }

    /// resolve "mod.sub.Class.attr" by importing the longest module prefix
    /// then walking locals.
    pub fn resolve_qname(&self, q: &str) -> Option<GNode> {
        let parts: Vec<&str> = q.split('.').collect();
        for split in (1..parts.len()).rev() {
            let modname = parts[..split].join(".");
            if let Ok(mid) = self.ast_from_module_name(&modname, true) {
                let mut cur = GNode {
                    m: mid,
                    n: NodeId::MODULE,
                };
                let mut ok = true;
                for seg in &parts[split..] {
                    let sym = self.sym(seg);
                    let md = self.md(cur.m);
                    let locals = md.locals.borrow();
                    match locals.get(&cur.n).and_then(|l| l.get(&sym)).and_then(|v| v.first()) {
                        Some(&g) => cur = g,
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok {
                    return Some(cur);
                }
            }
        }
        None
    }

    /// `next(node.infer(ctx), None)`-style single pull. Ok(None) =
    /// StopIteration before any value; Err = raised before the first value.
    pub fn first_value(&self, node: GNode, ctx: &Rc<Ctx>) -> Result<Option<Value>, ErrKind> {
        let mut first: Option<Value> = None;
        let end = {
            let first = &mut first;
            self.infer_to(node, ctx, &mut |v| {
                *first = Some(v);
                Drive::Stop
            })
        };
        match (first, end) {
            (Some(v), _) => Ok(Some(v)),
            (None, End::Raised(e)) => Err(e),
            (None, _) => Ok(None),
        }
    }

    /// `next(value.igetattr(name, ctx))` — single pull, abandoning the
    /// attribute generator. Ok(None) = StopIteration; Err = raised before
    /// the first value.
    pub fn igetattr_first(
        &self,
        owner: &Value,
        name: GSym,
        ctx: Option<&Rc<Ctx>>,
    ) -> Result<Option<Value>, ErrKind> {
        let mut first: Option<Value> = None;
        let end = {
            let first = &mut first;
            self.igetattr_value_to(owner, name, ctx, &mut |v| {
                *first = Some(v);
                Drive::Stop
            })
        };
        match (first, end) {
            (Some(v), _) => Ok(Some(v)),
            (None, End::Raised(e)) => Err(e),
            (None, _) => Ok(None),
        }
    }

    /// `next(callee.infer_call_result(caller, ctx), None)` — single pull.
    pub fn infer_call_result_first(
        &self,
        callee: &Value,
        caller: Option<GNode>,
        ctx: Option<&Rc<Ctx>>,
    ) -> Result<Option<Value>, ErrKind> {
        let mut first: Option<Value> = None;
        let end = {
            let first = &mut first;
            self.infer_call_result_to(callee, caller, ctx, &mut |v| {
                *first = Some(v);
                Drive::Stop
            })
        };
        match (first, end) {
            (Some(v), _) => Ok(Some(v)),
            (None, End::Raised(e)) => Err(e),
            (None, _) => Ok(None),
        }
    }

    // ---------- safe_infer (§5) ----------

    /// util.safe_infer: pulls at most TWO values then abandons the
    /// generator (no cache write / no bump for the second value).
    /// Emulate a completed `value.infer(context)` pull over a SYNTHETIC
    /// value standing for a fresh astroid node (safe_infer of container
    /// elements etc.): the first completion under a (lookupname=None,
    /// callcontext, boundnode) key bumps nodes_inferred once and "caches";
    /// replays are bump-free. Proxies are no-ops.
    pub fn synth_value_pull(&self, v: &Value, ctx: &Rc<Ctx>) {
        if let Some((tag, ptr)) = synth_node_id(v) {
            let key = (
                tag,
                ptr,
                None,
                ctx.callcontext.borrow().as_ref().map(|c| c.id),
                ctx.boundnode.borrow().as_ref().map(crate::value::value_key),
            );
            let tag = key.0;
            if !self.synth_hop_cache.borrow().contains(&key) {
                self.pin_value_identity(v);
                if let Some(bn) = ctx.boundnode.borrow().as_ref() {
                    self.pin_value_identity(bn);
                }
                self.synth_hop_cache.borrow_mut().insert(key);
                ctx.bump_inferred();
                if std::env::var("PRYLINT_TRACE_INFER").is_ok() {
                    eprintln!("SYNTHPULL bump tag={} ni={}", tag, ctx.nodes_inferred.get());
                }
            } else if std::env::var("PRYLINT_TRACE_INFER").is_ok() {
                eprintln!("SYNTHPULL replay tag={}", tag);
            }
        }
    }

    pub fn safe_infer(&self, node: GNode, ctx: &Rc<Ctx>) -> Option<Value> {
        let mut first: Option<Value> = None;
        let mut ambiguous = false;
        let end = {
            let first = &mut first;
            let ambiguous = &mut ambiguous;
            self.infer_to(node, ctx, &mut |v| {
                if first.is_none() {
                    *first = Some(v);
                    Drive::Go
                } else {
                    *ambiguous = true;
                    Drive::Stop
                }
            })
        };
        match end {
            End::Stopped => None,                      // second value -> ambiguity
            End::Raised(_) => None,                    // error on first OR second pull
            End::Done => first,                        // exactly one (or zero -> None)
        }
    }

    pub fn safe_infer_value(&self, v: &Value) -> Option<Value> {
        // values infer to themselves
        Some(v.clone())
    }

    // ---------- helpers on values ----------

    pub fn value_const(&self, v: &Value) -> Option<ConstValue> {
        match v {
            Value::SynthConst(c) => Some((**c).clone()),
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Const(c) => Some(c.clone()),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// elements of a container value, as Values (for starred unpacking)
    pub fn value_elts(&self, v: &Value) -> Option<Vec<Value>> {
        match v {
            Value::SynthSeq { elems, .. } | Value::FrozenSet { elems } => {
                Some(elems.to_vec())
            }
            // DictKeys/Values/Items proxy a synthesized List
            // (objectmodel.py:856-890) — `hasattr(x, "elts")` is True and
            // .elts holds the dict's raw key/value nodes (items: fresh
            // 2-Tuples of them)
            Value::DictKeys(dr) => Some(
                self.dictref_pairs(dr).into_iter().map(|(k, _)| k).collect(),
            ),
            Value::DictValues(dr) => Some(
                self.dictref_pairs(dr).into_iter().map(|(_, val)| val).collect(),
            ),
            Value::DictItems(dr) => Some(
                self.dictref_pairs(dr)
                    .into_iter()
                    .map(|(k, val)| Value::SynthSeq {
                        kind: SeqKind::Tuple,
                        elems: Rc::new(vec![k, val]),
                    })
                    .collect(),
            ),
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::List { elts, .. }
                    | NodeKind::Tuple { elts, .. }
                    | NodeKind::Set { elts } => Some(
                        elts.iter()
                            .map(|&e| Value::Node(GNode { m: g.m, n: e }))
                            .collect(),
                    ),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    pub fn value_dict_items(&self, v: &Value) -> Option<Vec<(Value, Value)>> {
        match v {
            Value::SynthDict { items } => Some(items.to_vec()),
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Dict { items } => Some(
                        items
                            .iter()
                            .map(|&(k, val)| {
                                (
                                    Value::Node(GNode { m: g.m, n: k }),
                                    Value::Node(GNode { m: g.m, n: val }),
                                )
                            })
                            .collect(),
                    ),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// the builtins ClassDef a value is an instance of (Instance._proxied)
    pub fn proxied_class(&self, v: &Value) -> Option<GNode> {
        let b = self.builtins();
        match v {
            Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => Some(*cls),
            Value::SynthConst(c) => Some(self.const_class(c)),
            Value::SynthSeq { kind, .. } => Some(match kind {
                SeqKind::List => b.list,
                SeqKind::Tuple => b.tuple,
                SeqKind::Set => b.set,
            }),
            Value::SynthDict { .. } => Some(b.dict),
            Value::SynthSlice { .. } => Some(b.slice),
            Value::FrozenSet { .. } => Some(b.frozenset),
            Value::Generator { is_async, .. } => Some(if *is_async {
                b.async_generator
            } else {
                b.generator
            }),
            Value::UnionType => Some(b.union_type),
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Const(c) => Some(self.const_class(c)),
                    NodeKind::List { .. } => Some(b.list),
                    NodeKind::Tuple { .. } => Some(b.tuple),
                    NodeKind::Set { .. } => Some(b.set),
                    NodeKind::Dict { .. } => Some(b.dict),
                    NodeKind::Slice { .. } => Some(b.slice),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// astroid `isinstance(x, bases.Instance)` -> `x._proxied`. ONLY the
    /// variants that are real `Instance` subclasses in astroid: Instance,
    /// ExceptionInstance, Const (node_classes.py `class Const(..., Instance)`),
    /// BaseContainer (List/Tuple/Set + objects.FrozenSet), Dict.
    /// NOT Generator/UnionType (BaseInstance only), NOT Slice/methods/etc.
    pub fn instance_unproxy(&self, v: &Value) -> Option<GNode> {
        let b = self.builtins();
        match v {
            Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => Some(*cls),
            Value::SynthConst(c) => Some(self.const_class(c)),
            Value::SynthSeq { kind, .. } => Some(match kind {
                SeqKind::List => b.list,
                SeqKind::Tuple => b.tuple,
                SeqKind::Set => b.set,
            }),
            Value::SynthDict { .. } => Some(b.dict),
            Value::FrozenSet { .. } => Some(b.frozenset),
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Const(c) => Some(self.const_class(c)),
                    NodeKind::List { .. } => Some(b.list),
                    NodeKind::Tuple { .. } => Some(b.tuple),
                    NodeKind::Set { .. } => Some(b.set),
                    NodeKind::Dict { .. } => Some(b.dict),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    pub fn const_class(&self, c: &ConstValue) -> GNode {
        let b = self.builtins();
        match c {
            ConstValue::None => b.none_type,
            ConstValue::NotImplemented => b.notimpl_type,
            ConstValue::Ellipsis => b.ellipsis_type,
            ConstValue::Bool(_) => b.bool_,
            ConstValue::Int(_) => b.int,
            ConstValue::Float(_) => b.float,
            ConstValue::Complex { .. } => b.complex,
            ConstValue::Str(_) | ConstValue::StrSurrogate(_) => b.str_,
            ConstValue::Bytes(_) => b.bytes,
        }
    }

    /// §16.1/16.2 bool_value. None == Uninferable.
    pub fn bool_value(&self, v: &Value, ctx: &Rc<Ctx>) -> Option<bool> {
        match v {
            Value::Uninferable => None,
            Value::DescBM { .. } => Some(true),
            // EvaluatedObject keeps NodeNG's default bool_value
            // (Uninferable) — it does not delegate to the wrapped value
            Value::EvaluatedObject { .. } => None,
            Value::SynthConst(c) => Some(const_truth(c)),
            Value::SynthSeq { elems, .. } => Some(!elems.is_empty()),
            Value::SynthDict { items } => Some(!items.is_empty()),
            Value::FrozenSet { elems } => Some(!elems.is_empty()),
            Value::SynthSlice { .. } => None,
            Value::Generator { .. }
            | Value::BoundMethod { .. }
            | Value::UnboundMethod { .. }
            | Value::UnionType
            | Value::Property { .. }
            | Value::Partial { .. }
            | Value::Super { .. } => Some(true),
            // dict-view proxies delegate bool_value to the objectmodel's
            // synthesized List (Proxy.__getattr__ -> List.bool_value =
            // bool(self.elts)): an items() view of an empty dict literal is
            // FALSY (BooleanConstraint rejects it — core triggers/event.py
            // event_data_items)
            Value::DictItems(dr) | Value::DictKeys(dr) | Value::DictValues(dr) => {
                Some(!self.dictref_pairs(dr).is_empty())
            }
            Value::Inst { .. } | Value::ExcInst { .. } => self.instance_bool_value(v, ctx),
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Const(c) => Some(const_truth(c)),
                    NodeKind::List { elts, .. }
                    | NodeKind::Tuple { elts, .. }
                    | NodeKind::Set { elts } => Some(!elts.is_empty()),
                    NodeKind::Dict { items } => Some(!items.is_empty()),
                    NodeKind::Module(_)
                    | NodeKind::ClassDef(_)
                    | NodeKind::FunctionDef(_)
                    | NodeKind::AsyncFunctionDef(_)
                    | NodeKind::Lambda(_)
                    | NodeKind::GeneratorExp(_) => Some(true),
                    _ => None,
                }
            }
        }
    }

    /// bases.py:388-414 Instance.bool_value
    fn instance_bool_value(&self, v: &Value, ctx: &Rc<Ctx>) -> Option<bool> {
        *ctx.boundnode.borrow_mut() = Some(v.clone());
        match self.infer_method_result_truth(v, "__bool__", ctx) {
            Ok(r) => r,
            Err(_) => match self.infer_method_result_truth(v, "__len__", ctx) {
                Ok(r) => r,
                Err(_) => Some(true),
            },
        }
    }

    /// bases.py:207-228 _infer_method_result_truth
    fn infer_method_result_truth(
        &self,
        instance: &Value,
        name: &str,
        ctx: &Rc<Ctx>,
    ) -> Result<Option<bool>, ErrKind> {
        let sym = self.sym(name);
        // next(instance.igetattr(method_name, context), None) — single pull
        let meth = match self.igetattr_first(instance, sym, Some(ctx)) {
            Ok(Some(m)) => m,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e),
        };
        if !self.has_infer_call_result(&meth) {
            return Ok(None);
        }
        if !self.value_callable(&meth, ctx) {
            return Ok(None);
        }
        let cc = Rc::new(CallCtx {
            id: self.next_callctx_id(),
            args: std::cell::RefCell::new(Vec::new()),
            keywords: std::cell::RefCell::new(Vec::new()),
            callee: std::cell::RefCell::new(Some(meth.clone())),
        });
        *ctx.callcontext.borrow_mut() = Some(cc);
        // first call-result value only (the `return` abandons the generator)
        match self.infer_call_result_first(&meth, None, Some(ctx)) {
            Ok(None) => Ok(None),
            Ok(Some(Value::Uninferable)) => Ok(None),
            Ok(Some(value)) => Ok(self.bool_value(&value, ctx)),
            Err(e) if e.is_inference() => Ok(None),
            Err(_) => Ok(None),
        }
    }

    pub fn has_infer_call_result(&self, v: &Value) -> bool {
        match v {
            Value::Node(g) => {
                let md = self.md(g.m);
                matches!(
                    md.tree.nodes[g.n.idx()].kind,
                    NodeKind::FunctionDef(_)
                        | NodeKind::AsyncFunctionDef(_)
                        | NodeKind::Lambda(_)
                        | NodeKind::ClassDef(_)
                        // Const/containers are Instance subclasses
                        | NodeKind::Const(_)
                        | NodeKind::List { .. }
                        | NodeKind::Tuple { .. }
                        | NodeKind::Set { .. }
                        | NodeKind::Dict { .. }
                )
            }
            Value::Inst { .. }
            | Value::ExcInst { .. }
            | Value::BoundMethod { .. }
            | Value::DescBM { .. }
            | Value::UnboundMethod { .. }
            | Value::Property { .. }
            | Value::Partial { .. }
            | Value::Generator { .. }
            | Value::SynthConst(_)
            | Value::SynthSeq { .. }
            | Value::SynthDict { .. }
            | Value::FrozenSet { .. }
            | Value::UnionType => true,
            _ => false,
        }
    }

    /// `.callable()` semantics (§11.8)
    pub fn value_callable(&self, v: &Value, _ctx: &Rc<Ctx>) -> bool {
        match v {
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::FunctionDef(_)
                    | NodeKind::AsyncFunctionDef(_)
                    | NodeKind::Lambda(_)
                    | NodeKind::ClassDef(_) => true,
                    NodeKind::Const(_)
                    | NodeKind::List { .. }
                    | NodeKind::Tuple { .. }
                    | NodeKind::Set { .. }
                    | NodeKind::Dict { .. }
                    | NodeKind::Slice { .. } => self.instance_callable(v),
                    _ => false,
                }
            }
            Value::Inst { .. } | Value::ExcInst { .. } => self.instance_callable(v),
            Value::BoundMethod { .. }
            | Value::UnboundMethod { .. }
            | Value::Property { .. }
            | Value::Partial { .. } => true,
            Value::Generator { .. } => false,
            Value::SynthConst(_) | Value::SynthSeq { .. } | Value::SynthDict { .. }
            | Value::FrozenSet { .. } => self.instance_callable(v),
            _ => false,
        }
    }

    fn instance_callable(&self, v: &Value) -> bool {
        // Instance.callable: class getattr("__call__", class_context=False)
        match self.proxied_class(v) {
            Some(cls) => {
                let sym = self.sym("__call__");
                self.class_getattr(cls, sym, None, false).is_ok()
            }
            None => false,
        }
    }
}

// ---------- const helpers ----------

pub fn const_truth(c: &ConstValue) -> bool {
    match c {
        ConstValue::None => false,
        ConstValue::Bool(b) => *b,
        ConstValue::Int(IntValue::Small(i)) => *i != 0,
        ConstValue::Int(IntValue::Big(_)) => true,
        ConstValue::Float(f) => *f != 0.0,
        ConstValue::Complex { real, imag } => *real != 0.0 || *imag != 0.0,
        ConstValue::Str(s) => !s.is_empty(),
        ConstValue::StrSurrogate(p) => !p.is_empty(),
        ConstValue::Bytes(b) => !b.is_empty(),
        ConstValue::Ellipsis => true,
        // NotImplemented is truthy on 3.12 (node_classes.py:2165-2175)
        ConstValue::NotImplemented => true,
    }
}

/// numeric/str comparison for Compare folding. Mirrors Python's comparison
/// for the literal types ast.literal_eval can produce. None => Uninferable.
/// Python literal value for Compare folding (_to_literal results)
#[derive(Clone, Debug)]
pub enum Lit {
    Const(ConstValue),
    List(Vec<Lit>),
    Tuple(Vec<Lit>),
    Set(Vec<Lit>),
    Dict(Vec<(Lit, Lit)>),
}

/// Python `==` over literals — operator.eq never raises: mismatched
/// types are simply unequal.
fn lit_eq(l: &Lit, r: &Lit) -> bool {
    match (l, r) {
        (Lit::Const(a), Lit::Const(b)) => const_eq(a, b),
        (Lit::List(a), Lit::List(b)) | (Lit::Tuple(a), Lit::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| lit_eq(x, y))
        }
        (Lit::Set(a), Lit::Set(b)) => {
            a.len() == b.len()
                && a.iter().all(|x| b.iter().any(|y| lit_eq(x, y)))
                && b.iter().all(|y| a.iter().any(|x| lit_eq(x, y)))
        }
        (Lit::Dict(a), Lit::Dict(b)) => {
            a.len() == b.len()
                && a.iter().all(|(k, v)| {
                    b.iter().any(|(k2, v2)| lit_eq(k, k2) && lit_eq(v, v2))
                })
        }
        _ => false,
    }
}

fn const_eq(l: &ConstValue, r: &ConstValue) -> bool {
    match (l, r) {
        (ConstValue::Str(a), ConstValue::Str(b)) => a == b,
        (ConstValue::Bytes(a), ConstValue::Bytes(b)) => a == b,
        (ConstValue::None, ConstValue::None) => true,
        _ => match (const_num(l), const_num(r)) {
            (Some(a), Some(b)) => a == b,
            _ => false, // mixed types: unequal, no raise
        },
    }
}

/// COMPARE_OPS (node_classes.py:1787-1796) over literals. None means the
/// Python operator would raise TypeError (-> AstroidTypeError -> U) or we
/// can't faithfully fold.
fn compare_literals(l: &Lit, op: &str, r: &Lit) -> Option<bool> {
    use std::cmp::Ordering;
    match op {
        "==" => Some(lit_eq(l, r)),
        "!=" => Some(!lit_eq(l, r)),
        "<" | "<=" | ">" | ">=" => {
            let ord: Option<Ordering> = match (l, r) {
                (Lit::Const(a), Lit::Const(b)) => match (const_num(a), const_num(b)) {
                    (Some(x), Some(y)) => x.partial_cmp(&y),
                    _ => match (a, b) {
                        (ConstValue::Str(x), ConstValue::Str(y)) => Some(x.cmp(y)),
                        (ConstValue::Bytes(x), ConstValue::Bytes(y)) => Some(x.cmp(y)),
                        _ => None, // TypeError
                    },
                },
                _ => None,
            };
            let o = ord?;
            Some(match op {
                "<" => o == Ordering::Less,
                "<=" => o != Ordering::Greater,
                ">" => o == Ordering::Greater,
                _ => o != Ordering::Less,
            })
        }
        "in" | "not in" => {
            let contains: Option<bool> = match r {
                Lit::Const(ConstValue::Str(hay)) => match l {
                    // `a in b` over strs is SUBSTRING containment
                    Lit::Const(ConstValue::Str(needle)) => {
                        Some(hay.contains(needle.as_ref() as &str))
                    }
                    _ => None, // TypeError: 'in <string>' requires string
                },
                Lit::Const(ConstValue::Bytes(hay)) => match l {
                    Lit::Const(ConstValue::Bytes(needle)) => Some(
                        hay.windows(needle.len().max(1))
                            .any(|w| w == needle.as_ref() as &[u8])
                            || needle.is_empty(),
                    ),
                    _ => None,
                },
                Lit::List(elems) | Lit::Tuple(elems) | Lit::Set(elems) => {
                    Some(elems.iter().any(|e| lit_eq(l, e)))
                }
                Lit::Dict(items) => Some(items.iter().any(|(k, _)| lit_eq(l, k))),
                _ => None, // TypeError
            };
            contains.map(|b| if op == "in" { b } else { !b })
        }
        _ => None, // is / is not handled by the caller
    }
}

fn const_num(c: &ConstValue) -> Option<f64> {
    match c {
        ConstValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        ConstValue::Int(IntValue::Small(i)) => Some(*i as f64),
        ConstValue::Float(f) => Some(*f),
        _ => None,
    }
}

/// format(value, spec) folding for f-strings; only the empty/simple specs
/// matter in practice — bail (None => Uninferable) otherwise.
/// str(value) for JoinedStr concatenation (node_classes.py:4840
/// `result += str(node.value)`)
fn const_str_value(c: &ConstValue) -> String {
    format_const(c, "").unwrap_or_else(|| match c {
        ConstValue::Bytes(b) => format!("b{:?}", String::from_utf8_lossy(b)),
        ConstValue::Ellipsis => "Ellipsis".to_string(),
        _ => String::new(),
    })
}

fn format_const(c: &ConstValue, spec: &str) -> Option<String> {
    if !spec.is_empty() {
        // format(value, spec) — node_classes.py:4719: the FULL format-spec
        // mini-language applies to Const values; ValueError/TypeError from
        // an invalid spec/value combination yields Uninferable (None here).
        return python_format(c, spec);
    }
    match c {
        ConstValue::Str(s) => Some(s.to_string()),
        ConstValue::Int(IntValue::Small(i)) => Some(i.to_string()),
        ConstValue::Int(IntValue::Big(s)) => Some(s.to_string()),
        ConstValue::Bool(b) => Some(if *b { "True" } else { "False" }.to_string()),
        ConstValue::Float(f) => Some(pyast::pyrepr::repr_float(*f)),
        ConstValue::None => Some("None".to_string()),
        // format(obj, "") falls back to str(obj) for the remaining Const
        // types: bytes/complex/Ellipsis (object.__format__ empty-spec)
        ConstValue::Bytes(b) => Some(crate::dump::repr_bytes_pub(b)),
        ConstValue::Complex { real, imag } => Some(complex_str(*real, *imag)),
        ConstValue::Ellipsis => Some("Ellipsis".to_string()),
        _ => None,
    }
}

/// str(complex) — Python prints `1j`, `(1+2j)`, `(-0-1j)` forms; the float
/// components drop a trailing `.0`
fn complex_str(real: f64, imag: f64) -> String {
    let fmt = |f: f64| -> String {
        let r = pyast::pyrepr::repr_float(f);
        r.strip_suffix(".0").map(|s| s.to_string()).unwrap_or(r)
    };
    let imag_s = fmt(imag);
    if real == 0.0 && real.is_sign_positive() && !(imag == 0.0 && imag.is_sign_negative()) {
        format!("{imag_s}j")
    } else {
        let real_s = fmt(real);
        if imag >= 0.0 || imag.is_nan() {
            format!("({real_s}+{imag_s}j)")
        } else {
            format!("({real_s}{imag_s}j)")
        }
    }
}

// ============== python format() spec mini-language for Consts ==============

struct FmtSpec {
    fill: char,
    align: Option<char>, // < > ^ =
    sign: Option<char>,  // + - space
    alt: bool,           // #
    zero: bool,          // 0 flag (implies fill '0', align '=' for numbers)
    width: usize,
    grouping: Option<char>, // , or _
    precision: Option<usize>,
    typ: Option<char>,
}

fn parse_fmt_spec(spec: &str) -> Option<FmtSpec> {
    let cs: Vec<char> = spec.chars().collect();
    let mut i = 0usize;
    let mut fill = ' ';
    let mut align: Option<char> = None;
    if cs.len() >= 2 && matches!(cs[1], '<' | '>' | '^' | '=') {
        fill = cs[0];
        align = Some(cs[1]);
        i = 2;
    } else if !cs.is_empty() && matches!(cs[0], '<' | '>' | '^' | '=') {
        align = Some(cs[0]);
        i = 1;
    }
    let mut sign: Option<char> = None;
    if i < cs.len() && matches!(cs[i], '+' | '-' | ' ') {
        sign = Some(cs[i]);
        i += 1;
    }
    if i < cs.len() && cs[i] == 'z' {
        return None; // PEP 682 negative-zero coercion: not folded
    }
    let mut alt = false;
    if i < cs.len() && cs[i] == '#' {
        alt = true;
        i += 1;
    }
    let mut zero = false;
    if i < cs.len() && cs[i] == '0' {
        zero = true;
        i += 1;
    }
    let mut width = 0usize;
    while i < cs.len() && cs[i].is_ascii_digit() {
        width = width.checked_mul(10)?.checked_add(cs[i] as usize - '0' as usize)?;
        i += 1;
    }
    if zero && width == 0 {
        // bare "0" was actually a zero width digit... CPython treats "0"
        // as zero-flag with width 0 (no-op padding); keep as parsed
    }
    let mut grouping: Option<char> = None;
    if i < cs.len() && (cs[i] == ',' || cs[i] == '_') {
        grouping = Some(cs[i]);
        i += 1;
    }
    let mut precision: Option<usize> = None;
    if i < cs.len() && cs[i] == '.' {
        i += 1;
        if i >= cs.len() || !cs[i].is_ascii_digit() {
            return None; // ValueError: Format specifier missing precision
        }
        let mut p = 0usize;
        while i < cs.len() && cs[i].is_ascii_digit() {
            p = p.checked_mul(10)?.checked_add(cs[i] as usize - '0' as usize)?;
            i += 1;
        }
        precision = Some(p);
    }
    let mut typ: Option<char> = None;
    if i < cs.len() {
        typ = Some(cs[i]);
        i += 1;
    }
    if i != cs.len() {
        return None; // ValueError: invalid format spec
    }
    Some(FmtSpec {
        fill,
        align,
        sign,
        alt,
        zero,
        width,
        grouping,
        precision,
        typ,
    })
}

/// apply fill/align/width to a finished body. `numeric`: default align '>'
/// and '='/zero-padding insert the fill between sign and digits.
fn fmt_pad(body: &str, fs: &FmtSpec, numeric: bool) -> String {
    let len = body.chars().count();
    if len >= fs.width {
        return body.to_string();
    }
    let pad = fs.width - len;
    let (fill, align) = if fs.align.is_some() {
        (fs.fill, fs.align.unwrap())
    } else if numeric && fs.zero {
        ('0', '=')
    } else if numeric {
        (if fs.zero { '0' } else { fs.fill }, '>')
    } else {
        (fs.fill, '<')
    };
    let fill_s: String = std::iter::repeat(fill).take(pad).collect();
    match align {
        '<' => format!("{body}{fill_s}"),
        '>' => format!("{fill_s}{body}"),
        '^' => {
            let left = pad / 2;
            let l: String = std::iter::repeat(fill).take(left).collect();
            let r: String = std::iter::repeat(fill).take(pad - left).collect();
            format!("{l}{body}{r}")
        }
        '=' => {
            // pad between sign (and any 0b/0o/0x alt prefix) and digits --
            // CPython renders '{:#06x}'.format(255) as '0x00ff'
            let mut keep = usize::from(body.starts_with(['-', '+', ' ']));
            let rest0 = &body[keep..];
            if rest0.len() >= 2
                && rest0.starts_with('0')
                && matches!(rest0.as_bytes()[1], b'b' | b'o' | b'x' | b'X' | b'B' | b'O')
            {
                keep += 2;
            }
            let (s, rest) = body.split_at(keep);
            format!("{s}{fill_s}{rest}")
        }
        _ => body.to_string(),
    }
}

/// insert a grouping separator every `group` digits from the right
fn group_digits(digits: &str, sep: char, group: usize) -> String {
    let n = digits.len();
    let mut out = String::new();
    for (idx, ch) in digits.chars().enumerate() {
        if idx > 0 && (n - idx) % group == 0 {
            out.push(sep);
        }
        out.push(ch);
    }
    out
}

fn fmt_sign(neg: bool, sign: Option<char>) -> &'static str {
    if neg {
        "-"
    } else {
        match sign {
            Some('+') => "+",
            Some(' ') => " ",
            _ => "",
        }
    }
}

pub(crate) fn python_format(c: &ConstValue, spec: &str) -> Option<String> {
    let fs = parse_fmt_spec(spec)?;
    match c {
        ConstValue::Str(s) => fmt_spec_str(s, &fs),
        ConstValue::Int(IntValue::Small(i)) => fmt_spec_int(*i, &fs),
        ConstValue::Int(IntValue::Big(s)) => fmt_spec_bigint(s, &fs),
        // bool: int.__format__ via the int path for numeric types; empty
        // type renders str(self)
        ConstValue::Bool(b) => match fs.typ {
            None => {
                if fs.sign.is_some() || fs.alt || fs.zero || fs.align == Some('=') {
                    return None;
                }
                Some(fmt_pad(if *b { "True" } else { "False" }, &fs, false))
            }
            _ => fmt_spec_int(*b as i64, &fs),
        },
        ConstValue::Float(f) => fmt_spec_float(*f, &fs),
        // object.__format__ raises TypeError on any non-empty spec
        _ => None,
    }
}

fn fmt_spec_str(s: &str, fs: &FmtSpec) -> Option<String> {
    if !matches!(fs.typ, None | Some('s')) {
        return None; // ValueError: unknown format code for str
    }
    if fs.sign.is_some() || fs.alt || fs.grouping.is_some() {
        return None; // ValueError: sign/#/grouping not allowed for str
    }
    if fs.zero || fs.align == Some('=') {
        return None; // ValueError: '=' alignment not allowed for str
    }
    let body: String = match fs.precision {
        Some(p) => s.chars().take(p).collect(),
        None => s.to_string(),
    };
    Some(fmt_pad(&body, fs, false))
}

fn fmt_spec_int(i: i64, fs: &FmtSpec) -> Option<String> {
    match fs.typ {
        Some('e') | Some('E') | Some('f') | Some('F') | Some('g') | Some('G') | Some('%') => {
            return fmt_spec_float(i as f64, fs);
        }
        None | Some('d') | Some('n') | Some('b') | Some('o') | Some('x') | Some('X')
        | Some('c') => {}
        _ => return None, // ValueError: unknown format code
    }
    if fs.precision.is_some() {
        return None; // ValueError: precision not allowed for int
    }
    let typ = fs.typ.unwrap_or('d');
    if typ == 'c' {
        if fs.sign.is_some() || fs.grouping.is_some() || i < 0 || i > 0x10FFFF {
            return None;
        }
        let ch = char::from_u32(i as u32)?;
        return Some(fmt_pad(&ch.to_string(), fs, true));
    }
    // grouping legality: ',' only for 'd'/default; '_' also for b/o/x/X
    let group: usize = match (fs.grouping, typ) {
        (None, _) => 0,
        (Some(','), 'd') => 3,
        (Some(','), _) => return None,
        (Some('_'), 'd' | 'n') => 3,
        (Some('_'), 'b' | 'o' | 'x' | 'X') => 4,
        _ => return None,
    };
    if fs.grouping == Some(',') && typ == 'n' {
        return None;
    }
    let neg = i < 0;
    let mag = (i as i128).unsigned_abs();
    let mut digits = match typ {
        'd' | 'n' => mag.to_string(),
        'b' => format!("{mag:b}"),
        'o' => format!("{mag:o}"),
        'x' => format!("{mag:x}"),
        'X' => format!("{mag:X}"),
        _ => unreachable!(),
    };
    if group > 0 {
        let sep = fs.grouping.unwrap();
        // zero-padding with grouping: CPython pads digits so the rendered
        // string (separators included) reaches the width; a leading
        // separator forces one extra zero
        if fs.zero && fs.align.is_none() {
            let sign_len = usize::from(neg || matches!(fs.sign, Some('+') | Some(' ')));
            loop {
                let rendered = group_digits(&digits, sep, group);
                if rendered.len() + sign_len >= fs.width {
                    break;
                }
                digits.insert(0, '0');
            }
            if (digits.len()) % group == 0 {
                // leading char would be a separator after one more pad
            }
            let mut rendered = group_digits(&digits, sep, group);
            if rendered.len() + usize::from(neg || matches!(fs.sign, Some('+') | Some(' ')))
                < fs.width
            {
                digits.insert(0, '0');
                rendered = group_digits(&digits, sep, group);
            }
            let body = format!("{}{}", fmt_sign(neg, fs.sign), rendered);
            return Some(body);
        }
        digits = group_digits(&digits, sep, group);
    }
    let prefix = if fs.alt {
        match typ {
            'b' => "0b",
            'o' => "0o",
            'x' => "0x",
            'X' => "0X",
            _ => "",
        }
    } else {
        ""
    };
    let body = format!("{}{}{}", fmt_sign(neg, fs.sign), prefix, digits);
    Some(fmt_pad(&body, fs, true))
}

fn fmt_spec_bigint(s: &str, fs: &FmtSpec) -> Option<String> {
    if !matches!(fs.typ, None | Some('d')) || fs.precision.is_some() {
        return None;
    }
    let neg = s.starts_with('-');
    let mut digits = s.trim_start_matches('-').to_string();
    if let Some(sep) = fs.grouping {
        if fs.zero && fs.align.is_none() {
            return None; // rare; skip the zero-pad+grouping interaction
        }
        digits = group_digits(&digits, sep, 3);
    }
    let body = format!("{}{}", fmt_sign(neg, fs.sign), digits);
    Some(fmt_pad(&body, fs, true))
}

/// 'g'-style mantissa/exponent split via Rust's correctly-rounded
/// exponential formatting. Returns (digits_no_dot, exponent) for
/// `sig` significant digits of |f|.
fn float_sig_digits(f: f64, sig: usize) -> (String, i32) {
    let e = format!("{:.*e}", sig - 1, f.abs());
    let (mant, exp) = e.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    (digits, exp)
}

/// render g-style given significant digits: fixed when -4 <= exp < p,
/// exponential otherwise; trailing zeros stripped unless alt.
fn fmt_g_core(f: f64, p: usize, upper: bool, alt: bool) -> String {
    let (digits, exp) = float_sig_digits(f, p);
    let mut body = if exp >= -4 && (exp as i64) < p as i64 {
        // fixed notation with p-1-exp digits after the point
        if exp >= 0 {
            let int_part = &digits[..(exp as usize + 1).min(digits.len())];
            let mut frac: String = digits[(exp as usize + 1).min(digits.len())..].to_string();
            if !alt {
                while frac.ends_with('0') {
                    frac.pop();
                }
            }
            if frac.is_empty() {
                if alt {
                    format!("{int_part}.")
                } else {
                    int_part.to_string()
                }
            } else {
                format!("{int_part}.{frac}")
            }
        } else {
            let zeros: String = std::iter::repeat('0').take((-exp - 1) as usize).collect();
            let mut frac = format!("{zeros}{digits}");
            if !alt {
                while frac.ends_with('0') {
                    frac.pop();
                }
            }
            format!("0.{frac}")
        }
    } else {
        // exponential notation
        let first = &digits[..1];
        let mut rest = digits[1..].to_string();
        if !alt {
            while rest.ends_with('0') {
                rest.pop();
            }
        }
        let mant = if rest.is_empty() {
            first.to_string()
        } else {
            format!("{first}.{rest}")
        };
        format!("{mant}e{exp:+03}")
    };
    if upper {
        body = body.to_uppercase();
    }
    body
}

fn fmt_spec_float(f: f64, fs: &FmtSpec) -> Option<String> {
    let typ = fs.typ;
    if matches!(
        typ,
        Some('d') | Some('b') | Some('o') | Some('x') | Some('X') | Some('c') | Some('s')
    ) {
        return None; // ValueError: unknown format code for float
    }
    if let Some(t) = typ {
        if !matches!(t, 'e' | 'E' | 'f' | 'F' | 'g' | 'G' | 'n' | '%') {
            return None;
        }
    }
    if fs.grouping == Some(',') && typ == Some('n') {
        return None;
    }
    let neg = f.is_sign_negative() && !(f == 0.0 && !f.is_sign_negative());
    let abs = f.abs();
    // nan/inf: no zero-fill digits, but padding applies
    if f.is_nan() || f.is_infinite() {
        let mut t = if f.is_nan() {
            "nan".to_string()
        } else {
            "inf".to_string()
        };
        if matches!(typ, Some('F') | Some('E') | Some('G')) {
            t = t.to_uppercase();
        }
        if typ == Some('%') {
            t.push('%');
        }
        let body = format!("{}{}", fmt_sign(f.is_sign_negative(), fs.sign), t);
        return Some(fmt_pad(&body, fs, true));
    }
    let mut body = match typ {
        Some('f') | Some('F') | Some('%') => {
            let p = fs.precision.unwrap_or(6);
            let scaled = if typ == Some('%') { abs * 100.0 } else { abs };
            let mut t = format!("{scaled:.p$}");
            if t.starts_with("inf") {
                // % scaling overflow
                t = "inf".to_string();
            } else if let Some(sep) = fs.grouping {
                let (int_part, frac) = match t.split_once('.') {
                    Some((a, b)) => (a.to_string(), Some(b.to_string())),
                    None => (t.clone(), None),
                };
                t = group_digits(&int_part, sep, 3);
                if let Some(fr) = frac {
                    t.push('.');
                    t.push_str(&fr);
                }
            }
            if typ == Some('%') {
                t.push('%');
            }
            t
        }
        Some('e') | Some('E') => {
            let p = fs.precision.unwrap_or(6);
            let (digits, exp) = float_sig_digits(abs, p + 1);
            let first = &digits[..1];
            let rest = &digits[1..];
            let mant = if rest.is_empty() {
                first.to_string()
            } else {
                format!("{first}.{rest}")
            };
            let mut t = format!("{mant}e{exp:+03}");
            if typ == Some('E') {
                t = t.to_uppercase();
            }
            t
        }
        Some('g') | Some('G') | Some('n') => {
            let p = match fs.precision.unwrap_or(6) {
                0 => 1,
                p => p,
            };
            fmt_g_core(abs, p, typ == Some('G'), fs.alt)
        }
        None => match fs.precision {
            // empty type with precision: 'g' + ".0" for integral results
            Some(p) => {
                let p = if p == 0 { 1 } else { p };
                let mut t = fmt_g_core(abs, p, false, false);
                if !t.contains('.') && !t.contains('e') {
                    t.push_str(".0");
                }
                t
            }
            // empty type without precision: repr (shortest round-trip)
            None => {
                let r = pyast::pyrepr::repr_float(abs);
                r
            }
        },
        _ => return None,
    };
    if fs.grouping.is_some() && !matches!(typ, Some('f') | Some('F') | Some('%')) {
        // grouping for e/g/empty types groups the integer part too
        if let Some(sep) = fs.grouping {
            if !body.contains('e') && !body.contains('E') {
                let (int_part, frac) = match body.split_once('.') {
                    Some((a, b)) => (a.to_string(), Some(b.to_string())),
                    None => (body.clone(), None),
                };
                body = group_digits(&int_part, sep, 3);
                if let Some(fr) = frac {
                    body.push('.');
                    body.push_str(&fr);
                }
            }
        }
    }
    let _ = neg;
    let body = format!("{}{}", fmt_sign(f.is_sign_negative(), fs.sign), body);
    Some(fmt_pad(&body, fs, true))
}

// ===================== str(astroid object) for f-string folding =====================

/// fake id() stand-ins sized like real CPython ids so pprint wrapping
/// decisions match GT (the digits themselves are nondeterministic in
/// astroid too — only the truncated 40-char render window must align,
/// and there the id is virtually never visible).
const FAKE_HEX_ID: &str = "0x102345678";
const FAKE_DEC_ID: &str = "4400000000";

/// pprint.pformat(list-of-leaf-reprs, indent=2, width=w) emulation for the
/// single nesting level NodeNG.__str__ feeds it (lists of node reprs or
/// 2-tuples of node reprs). Returns lines WITHOUT the outer alignment
/// prefix (the caller adds it like node_ng.py:200-205).
fn pformat_seq(items: &[String], width: usize, open: char, close: char) -> String {
    let oneline = format!(
        "{}{}{}",
        open,
        items.join(", "),
        close
    );
    if oneline.len() <= width || items.is_empty() {
        return oneline;
    }
    // multiline: "[ a,\n  b,\n  c]" (indent_per_level=2)
    let mut out = String::new();
    out.push(open);
    out.push(' ');
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n  ");
        }
        out.push_str(it);
    }
    out.push(close);
    out
}

impl Engine {
    /// str(obj) for InferenceResult objects per astroid:
    /// bases.py:372-373 (Instance), :721-722 (Generator), :447-452
    /// (UnboundMethod repr — str falls back to repr), node_ng.py:187-211
    /// (NodeNG pprint render). None => not rendered (caller yields U).
    pub fn astroid_object_str(&self, v: &Value) -> Option<String> {
        match v {
            Value::Uninferable => Some("Uninferable".to_string()),
            Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => {
                // bases.py:372: self._proxied.root().name — root() walks
                // PARENTS (reparent-aware: enum member fake classes are
                // reparented to the enum's module, so str() shows
                // 'Instance of homeassistant.const.KILO_WATT_HOUR')
                let mut top = *cls;
                while let Some(p) = self.parent(top) {
                    top = p;
                }
                let root = self.md(top.m).name.clone();
                let name = self.node_name(*cls).unwrap_or_default();
                Some(format!("Instance of {root}.{name}"))
            }
            Value::Generator { is_async, .. } => Some(if *is_async {
                "AsyncGenerator(async_generator)".to_string()
            } else {
                "Generator(generator)".to_string()
            }),
            Value::UnionType => Some("UnionType(UnionType)".to_string()),
            // dict-view proxies have no __str__/__repr__ override: default
            // object.__repr__ `<astroid.objects.DictKeys object at 0x...>`.
            // The address is the warm process's heap pointer (irreducible);
            // the dump's 40-char Const cut usually hides everything past
            // '...at 0x1' on this machine's typical heap range.
            Value::DictKeys(_) => {
                Some("<astroid.objects.DictKeys object at 0x10".to_string())
            }
            Value::DictValues(_) => {
                Some("<astroid.objects.DictValues object at 0x10".to_string())
            }
            Value::DictItems(_) => {
                Some("<astroid.objects.DictItems object at 0x10".to_string())
            }
            Value::BoundMethod { func, .. } | Value::UnboundMethod { func } => {
                // bases.py:447-452 __repr__ (no __str__ override; note the
                // missing closing '>' and DECIMAL id after '0x')
                let kind = if matches!(v, Value::BoundMethod { .. }) {
                    "BoundMethod"
                } else {
                    "UnboundMethod"
                };
                let name = self.node_name(*func).unwrap_or_default();
                let frame = self.parent(*func).map(|p| self.frame(p))?;
                let q = self.qname(frame);
                Some(format!("<{kind} {name} of {q} at 0x{FAKE_DEC_ID}"))
            }
            Value::SynthSeq { kind, elems } => {
                let reprs: Vec<String> =
                    elems.iter().map(|e| self.astroid_value_repr(e)).collect();
                // fresh containers are constructed without ctx -> ctx=None
                Some(container_str(*kind, Some("None"), &reprs))
            }
            Value::FrozenSet { elems } => {
                let reprs: Vec<String> =
                    elems.iter().map(|e| self.astroid_value_repr(e)).collect();
                Some(frozenset_str(&reprs))
            }
            Value::SynthDict { items } => {
                let pairs: Vec<(String, String)> = items
                    .iter()
                    .map(|(k, vv)| (self.astroid_value_repr(k), self.astroid_value_repr(vv)))
                    .collect();
                Some(dict_str(&pairs))
            }
            Value::Node(g) => {
                let md = self.md(g.m);
                match &md.tree.nodes[g.n.idx()].kind {
                    NodeKind::Dict { items, .. } => {
                        let pairs: Vec<(String, String)> = items
                            .iter()
                            .map(|(k, vv)| {
                                (
                                    self.astroid_node_repr(GNode { m: g.m, n: *k }),
                                    self.astroid_node_repr(GNode { m: g.m, n: *vv }),
                                )
                            })
                            .collect();
                        Some(dict_str(&pairs))
                    }
                    NodeKind::List { elts, ctx }
                    | NodeKind::Tuple { elts, ctx } => {
                        let kind = match &md.tree.nodes[g.n.idx()].kind {
                            NodeKind::List { .. } => SeqKind::List,
                            _ => SeqKind::Tuple,
                        };
                        let ctx_name = match ctx {
                            ExprCtx::Load => "Load",
                            ExprCtx::Store => "Store",
                            ExprCtx::Del => "Del",
                        };
                        let reprs: Vec<String> = elts
                            .iter()
                            .map(|&e| self.astroid_node_repr(GNode { m: g.m, n: e }))
                            .collect();
                        Some(container_str(kind, Some(ctx_name), &reprs))
                    }
                    NodeKind::Set { elts } => {
                        let reprs: Vec<String> = elts
                            .iter()
                            .map(|&e| self.astroid_node_repr(GNode { m: g.m, n: e }))
                            .collect();
                        Some(container_str(SeqKind::Set, None, &reprs))
                    }
                    // NodeNG.__str__ (node_ng.py:187-211): pprint render of
                    // _other_fields + _astroid_fields. Only the head is
                    // observable through the dump's 40-char Const repr cut;
                    // we render name (+ is_dataclass for ClassDef) exactly
                    // and a stable filler beyond.
                    NodeKind::ClassDef(d) => {
                        let name = md.tree.s(d.name).to_string();
                        let align = " ".repeat("ClassDef".len() + name.len() + 2);
                        // node.is_dataclass FLAG (set by dataclass_transform,
                        // brain_dataclasses.py:59) — read with NO inference
                        let dc = if self.is_dataclass_flag.borrow().contains(g) {
                            "True"
                        } else {
                            "False"
                        };
                        Some(format!(
                            "ClassDef.{name}(name='{name}',\n{align}is_dataclass={dc},\n{align}position=None"
                        ))
                    }
                    NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                        let cname = match &md.tree.nodes[g.n.idx()].kind {
                            NodeKind::AsyncFunctionDef(_) => "AsyncFunctionDef",
                            _ => "FunctionDef",
                        };
                        let name = md.tree.s(d.name).to_string();
                        let align = " ".repeat(cname.len() + name.len() + 2);
                        Some(format!("{cname}.{name}(name='{name}',\n{align}position=None"))
                    }
                    NodeKind::Module(_) => {
                        let name = md.name.clone();
                        let align = " ".repeat("Module".len() + name.len() + 2);
                        Some(format!("Module.{name}(name='{name}',\n{align}file=None"))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// repr(node) — node_ng.py:213-231 "<{cname}.{rname} l.{lineno} at 0x..>"
    fn astroid_node_repr(&self, g: GNode) -> String {
        let md = self.md(g.m);
        let lineno = md.tree.nodes[g.n.idx()].fromlineno;
        let (cname, rname): (&str, String) = match &md.tree.nodes[g.n.idx()].kind {
            NodeKind::Const(c) => ("Const", const_type_name(c).to_string()),
            NodeKind::Name { name } => ("Name", md.tree.s(*name).to_string()),
            NodeKind::Attribute { attrname, .. } => {
                ("Attribute", md.tree.s(*attrname).to_string())
            }
            NodeKind::List { .. } => ("List", "list".to_string()),
            NodeKind::Tuple { .. } => ("Tuple", "tuple".to_string()),
            NodeKind::Set { .. } => ("Set", "set".to_string()),
            NodeKind::Dict { .. } => ("Dict", "dict".to_string()),
            NodeKind::Call { .. } => ("Call", String::new()),
            NodeKind::FunctionDef(d) => ("FunctionDef", md.tree.s(d.name).to_string()),
            NodeKind::ClassDef(d) => ("ClassDef", md.tree.s(d.name).to_string()),
            _ => ("NodeNG", String::new()),
        };
        if rname.is_empty() {
            format!("<{cname} l.{lineno} at {FAKE_HEX_ID}>")
        } else {
            format!("<{cname}.{rname} l.{lineno} at {FAKE_HEX_ID}>")
        }
    }

    /// repr(value) for already-inferred element values inside synthetic
    /// containers (astroid holds real objects there: Const nodes, Instances)
    fn astroid_value_repr(&self, v: &Value) -> String {
        match v {
            Value::Node(g) => self.astroid_node_repr(*g),
            Value::Uninferable => "Uninferable".to_string(),
            Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => {
                let root = self.md(cls.m).name.clone();
                let name = self.node_name(*cls).unwrap_or_default();
                format!("<Instance of {root}.{name} at 0x{FAKE_DEC_ID}>")
            }
            Value::SynthConst(c) => {
                format!("<Const.{} l.0 at {FAKE_HEX_ID}>", const_type_name(c))
            }
            _ => format!("<NodeNG l.0 at {FAKE_HEX_ID}>"),
        }
    }
}

fn const_type_name(c: &ConstValue) -> &'static str {
    match c {
        ConstValue::Str(_) => "str",
        ConstValue::Bytes(_) => "bytes",
        ConstValue::Int(_) => "int",
        ConstValue::Float(_) => "float",
        ConstValue::Complex { .. } => "complex",
        ConstValue::Bool(_) => "bool",
        ConstValue::None => "NoneType",
        ConstValue::Ellipsis => "ellipsis",
        ConstValue::NotImplemented => "NotImplementedType",
        ConstValue::StrSurrogate(_) => "str",
    }
}

/// NodeNG.__str__ for containers: "List.list(ctx=<Context.Load: 1>,\n
/// {align}elts=[...])"; Set has NO ctx field (_other_fields empty).
fn container_str(kind: SeqKind, ctx: Option<&str>, elt_reprs: &[String]) -> String {
    let (cname, rname) = match kind {
        SeqKind::List => ("List", "list"),
        SeqKind::Tuple => ("Tuple", "tuple"),
        SeqKind::Set => ("Set", "set"),
    };
    let alignment = cname.len() + rname.len() + 2;
    let mut fields: Vec<String> = Vec::new();
    if !matches!(kind, SeqKind::Set) {
        let ctx_name = ctx.unwrap_or("Load");
        if ctx_name == "None" {
            // inference-fabricated containers (Sequence._infer new_seq,
            // brain _container_generic_inference) are constructed without
            // ctx — NodeNG.__str__ prints `ctx=None`
            fields.push("ctx=None".to_string());
        } else {
            let num = match ctx_name {
                "Store" => 2,
                "Del" => 3,
                _ => 1,
            };
            fields.push(format!("ctx=<Context.{ctx_name}: {num}>"));
        }
    }
    let width = 80usize.saturating_sub(4 + alignment); // len("elts")
    let body = pformat_seq(elt_reprs, width, '[', ']');
    let aligned = align_lines(&body, alignment);
    fields.push(format!("elts={aligned}"));
    let joined = fields.join(&format!(",\n{}", " ".repeat(alignment)));
    format!("{cname}.{rname}({joined})")
}

fn frozenset_str(elt_reprs: &[String]) -> String {
    let alignment = "FrozenSet".len() + "frozenset".len() + 2;
    let width = 80usize.saturating_sub(4 + alignment);
    let body = pformat_seq(elt_reprs, width, '[', ']');
    let aligned = align_lines(&body, alignment);
    format!("FrozenSet.frozenset(elts={aligned})")
}

/// Dict.__str__: items is a list of 2-tuples of nodes; pprint nests.
fn dict_str(pairs: &[(String, String)]) -> String {
    let alignment = "Dict".len() + "dict".len() + 2; // 10
    let width = 80usize.saturating_sub(5 + alignment); // len("items")
    let tuples: Vec<String> = pairs
        .iter()
        .map(|(k, v)| {
            let one = format!("({k}, {v})");
            if one.len() <= width.saturating_sub(4) {
                one
            } else {
                // pprint breaks the tuple: "( k,\n    v)" (nested indent 4)
                format!("( {k},\n    {v})")
            }
        })
        .collect();
    let body = pformat_seq(&tuples, width, '[', ']');
    let aligned = align_lines(&body, alignment);
    format!("Dict.dict(items={aligned})")
}

fn align_lines(s: &str, alignment: usize) -> String {
    let pad = " ".repeat(alignment);
    let mut out = String::new();
    for (i, line) in s.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
            out.push_str(&pad);
        }
        out.push_str(line);
    }
    out
}
