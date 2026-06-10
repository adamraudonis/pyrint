//! InferenceContext / CallContext — astroid/context.py (notes/07 §3).
//!
//! Python contexts are mutable objects aliased freely across the call
//! graph; `clone()` snapshots `path`/`constraints` and *shares* the
//! callcontext/boundnode/extra_context references and the nodes_inferred
//! counter cell. We mirror that with Rc<Ctx> + interior mutability.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::constraint::ConstraintSet;
use crate::value::{GNode, GSym, Value, NV};

pub struct CallCtx {
    /// unique per-construction id: the global inference cache keys
    /// callcontexts by *object identity* (context.py:19-23 note)
    pub id: u64,
    /// positional arguments: nodes from call sites, or already-inferred
    /// values for operator-protocol contexts (_get_binop_contexts).
    /// PartialFunction prepends filled args in place (objects.py:304-323).
    pub args: RefCell<Vec<NV>>,
    /// (keyword name | None for **expr, value node)
    pub keywords: RefCell<Vec<(Option<GSym>, GNode)>>,
    pub callee: RefCell<Option<Value>>,
}

pub struct Ctx {
    /// recursion guard: (node, lookupname) pairs (context.py:112-121)
    pub path: RefCell<FxHashSet<(GNode, Option<GSym>)>>,
    pub lookupname: Cell<Option<GSym>>,
    pub callcontext: RefCell<Option<Rc<CallCtx>>>,
    pub boundnode: RefCell<Option<Value>>,
    /// Call._infer._populate_context_lookup map (shared via Rc)
    pub extra_context: RefCell<Rc<FxHashMap<GNode, Rc<Ctx>>>>,
    /// name -> per-If-statement constraints (constraint.py)
    pub constraints: RefCell<FxHashMap<GSym, Rc<ConstraintSet>>>,
    /// shared across clones (context.py:49-98); max 100
    pub nodes_inferred: Rc<Cell<u32>>,
}

pub const MAX_INFERRED: u32 = 100;
/// AstroidManager.max_inferable_values (manager.py:63)
pub const MAX_INFERABLE_VALUES: usize = 100;

impl Ctx {
    pub fn new() -> Rc<Ctx> {
        Rc::new(Ctx {
            path: RefCell::new(FxHashSet::default()),
            lookupname: Cell::new(None),
            callcontext: RefCell::new(None),
            boundnode: RefCell::new(None),
            extra_context: RefCell::new(Rc::new(FxHashMap::default())),
            constraints: RefCell::new(FxHashMap::default()),
            nodes_inferred: Rc::new(Cell::new(0)),
        })
    }

    /// context.py:123-136 clone()
    pub fn clone_ctx(self: &Rc<Ctx>) -> Rc<Ctx> {
        Rc::new(Ctx {
            path: RefCell::new(self.path.borrow().clone()),
            lookupname: Cell::new(None),
            callcontext: RefCell::new(self.callcontext.borrow().clone()),
            boundnode: RefCell::new(self.boundnode.borrow().clone()),
            extra_context: RefCell::new(self.extra_context.borrow().clone()),
            constraints: RefCell::new(self.constraints.borrow().clone()),
            nodes_inferred: Rc::clone(&self.nodes_inferred),
        })
    }

    /// context.py:112-121 push: true => already visiting (produce nothing)
    pub fn push(&self, node: GNode) -> bool {
        let key = (node, self.lookupname.get());
        !self.path.borrow_mut().insert(key)
    }

    pub fn bump_inferred(&self) {
        self.nodes_inferred.set(self.nodes_inferred.get() + 1);
    }

    /// context.py:144-154 is_empty (inference-tip cache normalization)
    pub fn is_empty(&self) -> bool {
        self.path.borrow().is_empty()
            && self.nodes_inferred.get() == 0
            && self.callcontext.borrow().is_none()
            && self.boundnode.borrow().is_none()
            && self.lookupname.get().is_none()
            && self.extra_context.borrow().is_empty()
            && self.constraints.borrow().is_empty()
    }
}

/// context.py:184-189 copy_context
pub fn copy_context(ctx: Option<&Rc<Ctx>>) -> Rc<Ctx> {
    match ctx {
        Some(c) => c.clone_ctx(),
        None => Ctx::new(),
    }
}

/// context.py:192-204 bind_context_to_node
pub fn bind_context_to_node(ctx: Option<&Rc<Ctx>>, node: Value) -> Rc<Ctx> {
    let c = copy_context(ctx);
    *c.boundnode.borrow_mut() = Some(node);
    c
}
