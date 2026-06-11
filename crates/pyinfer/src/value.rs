//! The inference value model (astroid bases.py / objects.py proxies +
//! AST nodes inferring to themselves + Uninferable). notes/07 §1-2.

use std::rc::Rc;

use pyast::tree::ConstValue;
use pyast::NodeId;

pub type GSym = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModId(pub u32);

/// Global node reference: (module index in Engine.mods, node in that tree).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GNode {
    pub m: ModId,
    pub n: NodeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SeqKind {
    List,
    Tuple,
    Set,
}

/// An inference result. `Node` covers every AST node that infers to itself
/// (Const, containers, ClassDef, FunctionDef, Module, Slice, ...). The
/// `Synth*` variants stand in for the fresh nodes astroid fabricates during
/// inference (const_factory results, implicit-return Const(None), unpacked
/// vararg tuples, brain container builds...). Fresh Python objects are never
/// identical to each other, hence Synth values never dedupe in path_wrapper.
/// Identity of a bases.Instance python OBJECT. Instances have no __eq__,
/// so the inference-cache boundnode slot keys them by id(); each
/// instantiate_class() call creates a fresh object, while cache replay
/// yields the SAME object. Clones of a Value preserve the id (same
/// python object).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InstId(pub u64);

thread_local! {
    static NEXT_INST_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

pub fn fresh_inst_id() -> InstId {
    NEXT_INST_ID.with(|c| {
        let v = c.get();
        c.set(v + 1);
        InstId(v)
    })
}

#[derive(Debug, Clone)]
pub enum Value {
    Uninferable,
    Node(GNode),
    SynthConst(Rc<ConstValue>),
    /// fresh List/Tuple/Set with already-inferred elements
    SynthSeq { kind: SeqKind, elems: Rc<Vec<Value>> },
    /// fresh Dict with already-inferred items
    SynthDict { items: Rc<Vec<(Value, Value)>> },
    /// fresh Slice (brain infer_slice); bounds as const values
    SynthSlice { bounds: Rc<[Option<ConstValue>; 3]> },
    /// objects.FrozenSet
    FrozenSet { elems: Rc<Vec<Value>> },
    /// nodes.EvaluatedObject (node_classes.py:5010-5028): container-tip
    /// elements wrap their safe-inferred value
    /// (brain_builtin_inference.py:277-285). `_infer` yields the inner
    /// value; the node has NO getitem/itered, so loop-unpack getitem on an
    /// element raises AttributeError -> continue (protocols.py:268-276).
    EvaluatedObject { value: Rc<Value> },
    /// bases.Instance of a ClassDef
    Inst { cls: GNode, id: InstId },
    /// objects.ExceptionInstance (instance_attrs live in Engine.exc_iattrs
    /// keyed by an id when needed; the common case carries none).
    /// `id` is python OBJECT IDENTITY like Inst: astroid materializes a
    /// fresh ExceptionInstance per inference, so cache boundnode keys
    /// never merge distinct receivers (cache replays clone the SAME id).
    ExcInst { cls: GNode, id: InstId },
    BoundMethod { func: GNode, bound: Rc<Value> },
    /// FunctionModel.attr___get__ DescriptorBoundMethod
    /// (objectmodel.py:352-460): renders/keys like a BoundMethod on the
    /// wrapped function; CALLING it performs descriptor binding (1-2 args;
    /// yields the inner BM as-is, or a BM bound to the first arg's class).
    DescBM { func: GNode, inner: Rc<Value> },
    UnboundMethod { func: GNode },
    /// bases.Generator/AsyncGenerator. `call_ctx` is the context captured
    /// at creation time (bases.py:698 `self._call_context =
    /// copy_context(generator_initial_context)`); infer_yield_types uses
    /// it, NOT the consumer's context (bases.py:703-704).
    Generator {
        func: GNode,
        is_async: bool,
        call_ctx: Rc<crate::ctx::Ctx>,
    },
    /// objects.Property wrapping a FunctionDef. `synth` marks the
    /// property(...) call-tip product (objects.Property named "<property>"
    /// under SYNTHETIC_ROOT — its qname is the synthetic root's, NOT the
    /// wrapped function's).
    Property { func: GNode, synth: bool },
    /// objects.PartialFunction (functools.partial brain)
    Partial {
        func: GNode,
        filled_args: Rc<Vec<GNode>>,
        filled_keywords: Rc<Vec<(GSym, GNode)>>,
        /// PartialFunction.parent = the partial(...) Call's parent
        /// (brain_functools.py:119) — determines FunctionDef.type ("method"
        /// when the partial is assigned in a class body)
        parent: Option<GNode>,
    },
    /// objects.Super
    Super {
        mro_pointer: GNode,
        mro_type: Rc<Value>,
        self_class: GNode,
        scope: GNode,
    },
    /// bases.UnionType (PEP 604)
    UnionType,
    DictItems(Rc<DictRef>),
    DictKeys(Rc<DictRef>),
    DictValues(Rc<DictRef>),
}

/// What dict a DictItems/Keys/Values proxy wraps.
#[derive(Debug, Clone)]
pub enum DictRef {
    Node(GNode),
    Synth(Rc<Vec<(Value, Value)>>),
}

impl Value {
    pub fn is_uninferable(&self) -> bool {
        matches!(self, Value::Uninferable)
    }
}

/// Identity key used for the global inference-cache `boundnode` slot and
/// path-wrapper dedup. astroid keys by Python object identity; node-backed
/// values map to GNode identity, proxies to structural identity (documented
/// approximation — see notes in graph.rs::infer_cache).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ValueKey {
    Uninferable,
    Node(GNode),
    Inst(GNode, InstId),
    ExcInst(GNode, InstId),
    BoundMethod(GNode, Box<ValueKey>),
    UnboundMethod(GNode),
    /// generator objects compare by identity in python: key includes the
    /// captured-context pointer (fresh per infer_call_result run; preserved
    /// through cache replay clones)
    Generator(GNode, bool, usize),
    Property(GNode),
    Partial(GNode),
    Super(usize),
    UnionType,
    /// synthetic nodes/proxies key by Rc POINTER identity (fresh python
    /// objects compare by id(); Value clones share the Rc => same object)
    Synth(u8, usize),
}

pub fn value_key(v: &Value) -> ValueKey {
    match v {
        Value::Uninferable => ValueKey::Uninferable,
        Value::Node(g) => ValueKey::Node(*g),
        Value::Inst { cls, id } => ValueKey::Inst(*cls, *id),
        Value::ExcInst { cls, id, .. } => ValueKey::ExcInst(*cls, *id),
        Value::BoundMethod { func, bound } => {
            ValueKey::BoundMethod(*func, Box::new(value_key(bound)))
        }
        Value::DescBM { func, inner } => {
            ValueKey::BoundMethod(*func, Box::new(value_key(inner)))
        }
        Value::UnboundMethod { func } => ValueKey::UnboundMethod(*func),
        Value::Generator {
            func,
            is_async,
            call_ctx,
        } => ValueKey::Generator(*func, *is_async, Rc::as_ptr(call_ctx) as usize),
        Value::Property { func, .. } => ValueKey::Property(*func),
        Value::Partial { func, .. } => ValueKey::Partial(*func),
        // objects.Super has no __eq__: astroid's cache boundnode keys use
        // the OBJECT id — every super() Call._infer builds a FRESH Super,
        // so keys never merge across distinct super() sites/evaluations.
        // The per-construction mro_type Rc is the identity (clones/cache
        // replays share it). A structural (mro_pointer, self_class) key
        // produced FALSE cache hits: the iron_os 498:17 super() property
        // chain replayed a [U] entry written under line 465's Super.
        Value::Super { mro_type, .. } => ValueKey::Super(Rc::as_ptr(mro_type) as usize),
        Value::UnionType => ValueKey::UnionType,
        Value::SynthConst(rc) => ValueKey::Synth(0, Rc::as_ptr(rc) as usize),
        Value::SynthSeq { elems, .. } => ValueKey::Synth(1, Rc::as_ptr(elems) as *const u8 as usize),
        Value::SynthDict { items } => ValueKey::Synth(2, Rc::as_ptr(items) as *const u8 as usize),
        Value::SynthSlice { bounds } => ValueKey::Synth(3, Rc::as_ptr(bounds) as usize),
        Value::FrozenSet { elems } => ValueKey::Synth(4, Rc::as_ptr(elems) as *const u8 as usize),
        Value::DictItems(rc) => ValueKey::Synth(5, Rc::as_ptr(rc) as usize),
        Value::DictKeys(rc) => ValueKey::Synth(6, Rc::as_ptr(rc) as usize),
        Value::DictValues(rc) => ValueKey::Synth(7, Rc::as_ptr(rc) as usize),
        Value::EvaluatedObject { value } => {
            ValueKey::Synth(8, Rc::as_ptr(value) as usize)
        }
    }
}

/// Node-or-value: getattr / assigned_stmts results. astroid passes raw AST
/// nodes around and infers them later; object-model lookups and protocol
/// shortcuts produce already-inferred values.
#[derive(Debug, Clone)]
pub enum NV {
    N(GNode),
    V(Value),
}

/// Exception taxonomy subset (astroid/exceptions.py, notes/07 Appendix A).
/// NameError IS an InferenceError subclass; Attribute is NOT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrKind {
    Inference,
    NameError,
    Attribute,
    /// stand-in for Python RecursionError (depth guard)
    Recursion,
    AstroidType,
    AstroidIndex,
    AstroidValue,
    NoDefault,
    TooManyLevels,
    Mro,
    Super,
    Building,
    UseDefault,
}

impl ErrKind {
    /// `except InferenceError` catches NameInferenceError too.
    pub fn is_inference(self) -> bool {
        matches!(self, ErrKind::Inference | ErrKind::NameError)
    }
}

/// Consumer's instruction to a streaming producer. `Stop` mirrors a Python
/// consumer dropping a suspended generator (`next()` then abandon): the
/// producer must unwind immediately — code "after the yield" (counter
/// bumps, cache writes) must NOT run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drive {
    Go,
    Stop,
}

/// How a streaming producer ended: clean StopIteration, consumer
/// abandonment, or a raised exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Done,
    Stopped,
    Raised(ErrKind),
}

impl End {
    pub fn err_opt(self) -> Option<ErrKind> {
        match self {
            End::Raised(e) => Some(e),
            _ => None,
        }
    }
}

/// Result of running a Python generator eagerly: the values yielded before
/// termination plus the exception (if any) that ended it. `err: None` means
/// clean StopIteration.
#[derive(Debug, Clone)]
pub struct Flow {
    pub vals: Vec<Value>,
    pub err: Option<ErrKind>,
}

impl Flow {
    pub fn ok(vals: Vec<Value>) -> Flow {
        Flow { vals, err: None }
    }
    pub fn one(v: Value) -> Flow {
        Flow {
            vals: vec![v],
            err: None,
        }
    }
    pub fn empty() -> Flow {
        Flow {
            vals: Vec::new(),
            err: None,
        }
    }
    pub fn err(e: ErrKind) -> Flow {
        Flow {
            vals: Vec::new(),
            err: Some(e),
        }
    }
    pub fn uninferable() -> Flow {
        Flow::one(Value::Uninferable)
    }
    pub fn is_err(&self) -> bool {
        self.err.is_some()
    }
    /// decorators.py:57-66 yes_if_nothing_inferred: a generator producing
    /// nothing yields one Uninferable; errors raised while computing the
    /// FIRST value propagate unchanged.
    pub fn yes_if_nothing(self) -> Flow {
        if self.vals.is_empty() && self.err.is_none() {
            Flow::uninferable()
        } else {
            self
        }
    }
    /// decorators.py:68-96 raise_if_nothing_inferred: empty StopIteration ->
    /// InferenceError; RecursionError on the first value -> InferenceError.
    pub fn raise_if_nothing(self) -> Flow {
        if self.vals.is_empty() {
            match self.err {
                None => Flow::err(ErrKind::Inference),
                Some(ErrKind::Recursion) => Flow::err(ErrKind::Inference),
                Some(e) => Flow::err(e),
            }
        } else {
            self
        }
    }
}
