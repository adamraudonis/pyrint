use rustpython_ast::{self as ast};
use crate::ast_visitor::AstContext;
use crate::errors::{E1120, E1205};

/// Track function signatures for argument checking
#[derive(Debug, Clone)]
pub struct FunctionSignature {
    pub name: String,
    pub min_args: usize,
    pub max_args: Option<usize>,
    pub required_args: Vec<String>,
    pub has_varargs: bool,
    pub has_kwargs: bool,
}

impl AstContext {
    /// Check function call arguments for E1120 errors
    pub fn check_function_call_args(&mut self, call: &ast::ExprCall) {
        let start = call.range.start();
        let (line, col) = self.offset_to_line_col(start);
        
        // Check for logging functions with E1205
        if let ast::Expr::Attribute(attr) = &*call.func {
            if let ast::Expr::Name(name) = &*attr.value {
                let obj_name = name.id.as_str();
                let method_name = attr.attr.as_str();
                
                // Check for logging calls
                if obj_name == "LOG" || obj_name == "logger" || obj_name == "logging" {
                    self.check_logging_format(call, method_name, line, col);
                }
            }
        }
        
        // Check for function calls - only check against signatures we've tracked
        if let ast::Expr::Name(name) = &*call.func {
            let func_name = name.id.to_string();
            
            // Check for functions we've seen defined (clone to avoid borrow issue)
            if let Some(sig) = self.function_signatures.get(&func_name).cloned() {
                self.check_call_against_signature(call, &sig, line, col);
            }
        }
    }
    
    /// Check logging format strings for E1205 errors
    fn check_logging_format(&mut self, call: &ast::ExprCall, method: &str, line: usize, col: usize) {
        // Common logging methods
        let format_methods = ["debug", "info", "warning", "warn", "w", "error", "e", "critical", "d", "i"];
        
        if !format_methods.contains(&method) {
            return;
        }
        
        // Get the format string (first argument)
        if let Some(first_arg) = call.args.first() {
            // Try to extract string value from Constant expression
            let format_str = match first_arg {
                ast::Expr::Constant(c) => {
                    match &c.value {
                        ast::Constant::Str(s) => Some(s.as_str()),
                        _ => None,
                    }
                }
                _ => None,
            };
            
            if let Some(format_str) = format_str {
                // Count format placeholders
                let placeholder_count = count_format_placeholders(format_str);
                
                // Count provided arguments (excluding the format string itself)
                let provided_args = call.args.len() - 1;
                
                // Check if there are more arguments than placeholders
                if provided_args > placeholder_count {
                    self.add_issue(
                        &E1205,
                        line,
                        col,
                        vec![],
                    );
                }
            }
        }
    }
    
    /// Check a function call against a known signature
    fn check_call_against_signature(&mut self, call: &ast::ExprCall, sig: &FunctionSignature, line: usize, col: usize) {
        let provided_args = call.args.len();
        let provided_kwargs = call.keywords.len();
        
        // Check minimum arguments - even functions with **kwargs need their required positional args
        if provided_args < sig.min_args {
            // Find which required arguments are missing
            for (i, arg_name) in sig.required_args.iter().enumerate() {
                if i >= provided_args {
                    // Check if this arg was provided as a keyword
                    let provided_as_kwarg = call.keywords.iter().any(|kw| {
                        kw.arg.as_ref().map_or(false, |arg| arg.as_str() == arg_name)
                    });
                    
                    if !provided_as_kwarg {
                        self.add_issue(
                            &E1120,
                            line,
                            col,
                            vec![arg_name.clone()],
                        );
                        break; // Only report the first missing arg
                    }
                }
            }
        }
    }
}

/// Count Python format string placeholders
fn count_format_placeholders(s: &str) -> usize {
    let mut count = 0;
    let mut chars = s.chars().peekable();
    
    while let Some(c) = chars.next() {
        if c == '%' {
            // Check if it's an escaped %
            if let Some(&next_c) = chars.peek() {
                if next_c == '%' {
                    chars.next(); // Skip escaped %
                } else if next_c != '(' && !next_c.is_whitespace() {
                    // It's a format placeholder like %s, %d, etc.
                    count += 1;
                }
            }
        } else if c == '{' {
            // Check for {} format strings (modern Python)
            if let Some(&next_c) = chars.peek() {
                if next_c == '}' || next_c.is_ascii_digit() || next_c == ':' {
                    // It's a format placeholder
                    count += 1;
                }
            }
        }
    }
    
    count
}