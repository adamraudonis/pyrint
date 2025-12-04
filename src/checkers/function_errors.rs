use rustpython_ast::{self as ast};
use crate::ast_visitor::AstContext;

pub fn check_abstract_class_instantiation(_context: &mut AstContext, _expr: &ast::ExprCall) {
    // TODO: Implement abstract class instantiation check
    // This requires tracking abstract base classes and their methods
}

pub fn check_reversed_sequence(_context: &mut AstContext, _expr: &ast::ExprCall) {
    // TODO: Implement reversed() argument validation
    // Check if the first argument to reversed() is a sequence type
}