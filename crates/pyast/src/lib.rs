//! astroid-equivalent AST: tree built from ruff's parser with astroid's
//! node taxonomy, positions (fromlineno/tolineno/col_offset), scopes and
//! locals dictionaries, matching astroid 4.0.4 semantics exactly.

pub mod source;

pub use source::SourceFile;
