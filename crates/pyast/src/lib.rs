//! astroid-equivalent AST: tree built from ruff's parser with astroid's
//! node taxonomy, positions (fromlineno/tolineno/col_offset), scopes and
//! locals dictionaries, matching astroid 4.0.4 semantics exactly.

pub mod build;
pub mod codecs_gen;
pub mod parse;
pub mod pyrepr;
pub mod source;
pub mod stdlib_wildcard;
pub mod tree;

pub use source::{decode_source, DecodeError, SourceFile};
pub use tree::{Node, NodeId, NodeKind, Tree};
