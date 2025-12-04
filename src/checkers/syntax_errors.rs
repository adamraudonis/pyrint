use rustpython_ast::{self as ast};
use crate::ast_visitor::AstContext;
use crate::errors::*;

pub fn check_duplicate_star_expressions(context: &mut AstContext, targets: &[ast::Expr]) {
    let mut star_count = 0;
    for target in targets {
        if matches!(target, ast::Expr::Starred(_)) {
            star_count += 1;
        }
    }
    
    if star_count > 1 {
        context.add_issue(
            &E0112,
            1,
            0,
            vec![],
        );
    }
}

pub fn check_star_assignment_context(context: &mut AstContext, expr: &ast::Expr) {
    match expr {
        ast::Expr::Starred(starred) => {
            let start = starred.range.start();
            let (line, col) = context.offset_to_line_col(start);
            context.add_issue(
                &E0114,
                line,
                col,
                vec![],
            );
        }
        _ => {}
    }
}