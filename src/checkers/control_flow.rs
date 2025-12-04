use rustpython_ast::{self as ast};
use crate::ast_visitor::AstContext;
use crate::errors::*;

pub fn check_break_continue_context(context: &mut AstContext, is_break: bool, line: usize, column: usize) {
    if context.in_loop == 0 {
        let msg = if is_break { "'break'" } else { "'continue'" };
        context.add_issue(
            &E0103,
            line,
            column,
            vec![msg.to_string()],
        );
    }
}