//! pyinfer — port of the astroid 4.0.4 inference engine.
//!
//! Spec: reference/notes/07-inference.md (+ 00-architecture.md §pyinfer).
//! Ultimate truth: reference/astroid/astroid (cited file:line in comments).
//!
//! Single-threaded by design for the --dump-infer differential phase:
//! astroid's caches are process-global and order-sensitive, so we mirror
//! that with one `Engine` owning every cache.

pub mod brains;
pub mod calls;
pub mod constraint;
pub mod ctx;
pub mod dump;
pub mod getattr;
pub mod graph;
pub mod infer;
pub mod intern;
pub mod lookup;
pub mod numpy_templates;
pub mod protocols;
pub mod pyenv;
pub mod snapshot;
pub mod transforms;
pub mod treeutil;
pub mod value;

pub use ctx::{CallCtx, Ctx};
pub use graph::Engine;
pub use value::{ErrKind, Flow, GNode, GSym, ModId, Value, NV};
