use rustpython_ast::{self as ast};
use crate::ast_visitor::AstContext;
use crate::errors::*;

pub fn check_global_usage(context: &mut AstContext, name: &str, line: usize, column: usize) {
    // Check if name was used before global declaration
    // This requires tracking name usage order
    if context.defined_names.contains_key(name) {
        context.add_issue(
            &E0118,
            line,
            column,
            vec![name.to_string()],
        );
    }
}