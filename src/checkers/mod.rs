pub mod syntax_errors;
pub mod function_errors;
pub mod control_flow;
pub mod scope_errors;
pub mod call_errors;

pub use syntax_errors::*;
pub use function_errors::*;
pub use control_flow::*;
pub use scope_errors::*;
pub use call_errors::*;