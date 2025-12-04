use rustpython_ast::{self as ast};
use rustpython_parser::{parse, Mode, text_size::TextSize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::errors::{ErrorCode, Issue};
use crate::checkers::call_errors::FunctionSignature;

pub struct AstContext {
    pub file_path: std::path::PathBuf,
    pub source: String,
    pub issues: Vec<Issue>,
    pub in_function: bool,
    pub in_class: bool,
    pub in_loop: usize,
    pub in_generator: bool,
    pub in_init: bool,
    pub defined_names: HashMap<String, (usize, usize)>,
    pub class_methods: HashMap<String, (usize, usize)>,
    pub global_names: HashSet<String>,
    pub nonlocal_names: HashSet<String>,
    pub function_args: HashSet<String>,
    pub current_class: Option<String>,
    pub local_vars: HashSet<String>,
    pub conditionally_defined: HashMap<String, bool>,
    pub definitely_defined: HashSet<String>,
    pub function_signatures: HashMap<String, FunctionSignature>,
    pub imports: HashMap<String, String>, // Maps imported name to module path
    pub module_conditionally_defined: HashSet<String>, // Module-level variables that are conditionally defined
}

impl AstContext {
    pub fn new(file_path: &Path, source: String) -> Self {
        Self {
            file_path: file_path.to_path_buf(),
            source,
            issues: Vec::new(),
            in_function: false,
            in_class: false,
            in_loop: 0,
            in_generator: false,
            in_init: false,
            defined_names: HashMap::new(),
            class_methods: HashMap::new(),
            global_names: HashSet::new(),
            nonlocal_names: HashSet::new(),
            function_args: HashSet::new(),
            current_class: None,
            local_vars: HashSet::new(),
            conditionally_defined: HashMap::new(),
            definitely_defined: HashSet::new(),
            function_signatures: HashMap::new(),
            imports: HashMap::new(),
            module_conditionally_defined: HashSet::new(),
        }
    }

    pub fn add_issue(&mut self, code: &ErrorCode, line: usize, column: usize, args: Vec<String>) {
        let message = if args.is_empty() {
            code.message_template.to_string()
        } else {
            let mut msg = code.message_template.to_string();
            for (i, arg) in args.iter().enumerate() {
                msg = msg.replace(&format!("{{{}}}", i), arg);
                msg = msg.replace("{}", arg);
            }
            msg
        };

        self.issues.push(Issue::new(
            code.code.to_string(),
            message,
            self.file_path.clone(),
            line,
            column,
            code.symbol.to_string(),
        ));
    }

    pub fn parse_and_check(&mut self) -> Result<(), String> {
        let ast_result = parse(&self.source, Mode::Module, "<module>");
        
        match ast_result {
            Ok(ast_module) => {
                self.visit_module(ast_module);
                Ok(())
            }
            Err(e) => {
                let (line, col) = self.offset_to_line_col(e.offset);
                self.add_issue(
                    &crate::errors::E0001,
                    line,
                    col,
                    vec![e.error.to_string()],
                );
                Err(format!("Syntax error: {}", e.error))
            }
        }
    }

    fn visit_module(&mut self, module: ast::Mod) {
        match module {
            ast::Mod::Module(ast::ModModule { body, .. }) => {
                for stmt in body {
                    self.visit_stmt(stmt);
                }
            }
            _ => {}
        }
    }

    fn visit_stmt(&mut self, stmt: ast::Stmt) {
        use ast::Stmt::*;
        
        match stmt {
            FunctionDef(func) => self.visit_function_def(func),
            AsyncFunctionDef(func) => self.visit_async_function_def(func),
            ClassDef(cls) => self.visit_class_def(cls),
            Return(ret) => self.visit_return(ret),
            For(for_stmt) => self.visit_for(for_stmt),
            AsyncFor(for_stmt) => self.visit_async_for(for_stmt),
            While(while_stmt) => self.visit_while(while_stmt),
            If(if_stmt) => self.visit_if(if_stmt),
            With(with_stmt) => self.visit_with(with_stmt),
            AsyncWith(with_stmt) => self.visit_async_with(with_stmt),
            Try(try_stmt) => self.visit_try(try_stmt),
            Global(global_stmt) => self.visit_global(global_stmt),
            Nonlocal(nonlocal_stmt) => self.visit_nonlocal(nonlocal_stmt),
            Expr(expr_stmt) => self.visit_expr_stmt(expr_stmt),
            Pass(_) => {}
            Break(brk) => self.visit_break(brk),
            Continue(cont) => self.visit_continue(cont),
            Assign(assign) => self.visit_assign(assign),
            AnnAssign(ann_assign) => self.visit_ann_assign(ann_assign),
            Import(import) => self.visit_import(import),
            ImportFrom(import_from) => self.visit_import_from(import_from),
            _ => {}
        }
    }

    fn visit_function_def(&mut self, func: ast::StmtFunctionDef) {
        let start = func.range.start();
        let (line, col) = self.offset_to_line_col(start);
        let func_name = func.name.to_string();
        
        if self.in_class {
            // Check for duplicate methods within the class
            if let Some(class_name) = &self.current_class {
                let qualified_name = format!("{}::{}", class_name, func_name);
                if let Some(prev_loc) = self.class_methods.get(&qualified_name) {
                    self.add_issue(
                        &crate::errors::E0102,
                        line,
                        col,
                        vec![format!("method already defined line {}", prev_loc.0)],
                    );
                } else {
                    self.class_methods.insert(qualified_name, (line, col));
                }
            }
        } else if !self.in_function {
            // Only check for duplicate functions at module level (not inside other functions)
            if let Some(prev_loc) = self.defined_names.get(&func_name) {
                self.add_issue(
                    &crate::errors::E0102,
                    line,
                    col,
                    vec![format!("function already defined line {}", prev_loc.0)],
                );
            } else {
                self.defined_names.insert(func_name.clone(), (line, col));
            }
        }
        // If we're inside a function, nested functions are allowed to have the same name

        // Store function signature for argument checking
        if !self.in_class && !self.in_function { // Only track top-level functions for now
            use crate::checkers::call_errors::FunctionSignature;
            
            let mut required_args = Vec::new();
            let mut min_args = 0;
            
            // Count required positional arguments
            for arg in &func.args.posonlyargs {
                if arg.default.is_none() {
                    let arg_name = arg.def.arg.to_string();
                    required_args.push(arg_name);
                    min_args += 1;
                }
            }
            for arg in &func.args.args {
                if arg.default.is_none() {
                    let arg_name = arg.def.arg.to_string();
                    required_args.push(arg_name);
                    min_args += 1;
                }
            }
            
            let max_args = if func.args.vararg.is_some() {
                None
            } else {
                Some(func.args.posonlyargs.len() + func.args.args.len() + func.args.kwonlyargs.len())
            };
            
            let signature = FunctionSignature {
                name: func_name.clone(),
                min_args,
                max_args,
                required_args,
                has_varargs: func.args.vararg.is_some(),
                has_kwargs: func.args.kwarg.is_some(),
            };
            
            self.function_signatures.insert(func_name.clone(), signature);
        }
        
        let is_init = func.name.as_str() == "__init__";
        let prev_in_function = self.in_function;
        let prev_in_init = self.in_init;
        let prev_in_generator = self.in_generator;
        let prev_args = self.function_args.clone();
        let prev_local_vars = self.local_vars.clone();
        let prev_conditionally = self.conditionally_defined.clone();
        let prev_definitely = self.definitely_defined.clone();
        
        // E0211: Method has no argument
        // E0213: Method should have self as first argument
        // Only check for methods directly in a class, not nested functions inside methods
        if self.in_class && !self.in_function {
            // Check if function has @staticmethod or @classmethod decorator
            let has_staticmethod = func.decorator_list.iter().any(|decorator| {
                self.is_decorator_name(decorator, "staticmethod")
            });
            let has_classmethod = func.decorator_list.iter().any(|decorator| {
                self.is_decorator_name(decorator, "classmethod")
            });
            
            if func.args.posonlyargs.is_empty() && 
               func.args.args.is_empty() && 
               func.args.vararg.is_none() && 
               func.args.kwonlyargs.is_empty() && 
               func.args.kwarg.is_none() {
                // E0211: Method has no argument - but only if not staticmethod
                if !has_staticmethod {
                    self.add_issue(
                        &crate::errors::E0211,
                        line,
                        col,
                        vec![func.name.to_string()],
                    );
                }
            } else if !has_staticmethod {
                // Check for self/cls as first argument
                let first_arg = func.args.posonlyargs.first()
                    .or_else(|| func.args.args.first());
                    
                if let Some(arg) = first_arg {
                    let arg_name = match arg {
                        ast::ArgWithDefault { def, .. } => def.arg.to_string(),
                    };
                    let expected_name = if has_classmethod { "cls" } else { "self" };
                    if arg_name != expected_name && !has_staticmethod {
                        // E0213: Method should have self/cls as first argument
                        self.add_issue(
                            &crate::errors::E0213,
                            line,
                            col,
                            vec![func.name.to_string()],
                        );
                    }
                } else if !has_staticmethod {
                    // No arguments at all for non-static method
                    self.add_issue(
                        &crate::errors::E0213,
                        line,
                        col,
                        vec![func.name.to_string()],
                    );
                }
            }
        }
        
        // Now set in_function to true for the body processing
        self.in_function = true;
        self.in_init = is_init;
        self.in_generator = false;
        self.function_args.clear();

        let mut seen_args = HashSet::new();
        // Check all argument types for duplicates
        for arg in func.args.posonlyargs.iter()
            .chain(func.args.args.iter())
            .chain(func.args.kwonlyargs.iter()) 
        {
            let arg_name = match arg {
                ast::ArgWithDefault { def, .. } => def.arg.to_string(),
            };
            if !seen_args.insert(arg_name.clone()) {
                // For simplicity, report at function location since we don't have arg range
                self.add_issue(
                    &crate::errors::E0108,
                    line,
                    col,
                    vec![arg_name.clone()],
                );
            }
            self.function_args.insert(arg_name);
        }
        
        if let Some(arg) = &func.args.vararg {
            let arg_name = arg.arg.to_string();
            if !seen_args.insert(arg_name.clone()) {
                self.add_issue(
                    &crate::errors::E0108,
                    line,
                    col,
                    vec![arg_name.clone()],
                );
            }
            self.function_args.insert(arg_name);
        }
        
        if let Some(arg) = &func.args.kwarg {
            let arg_name = arg.arg.to_string();
            if !seen_args.insert(arg_name.clone()) {
                self.add_issue(
                    &crate::errors::E0108,
                    line,
                    col,
                    vec![arg_name.clone()],
                );
            }
            self.function_args.insert(arg_name);
        }

        let has_yield = self.check_for_yield(&func.body);
        let has_return_value = self.check_for_return_value(&func.body);
        
        if has_yield {
            self.in_generator = true;
            if is_init {
                self.add_issue(&crate::errors::E0100, line, col, vec![]);
            }
        } else if is_init && has_return_value {
            // Report E0101 at function definition line like Pylint does
            self.add_issue(&crate::errors::E0101, line, col, vec![]);
        }

        for stmt in func.body {
            self.visit_stmt(stmt);
        }

        self.in_function = prev_in_function;
        self.in_init = prev_in_init;
        self.in_generator = prev_in_generator;
        self.function_args = prev_args;
        self.local_vars = prev_local_vars;
        self.conditionally_defined = prev_conditionally;
        self.definitely_defined = prev_definitely;
    }

    fn visit_async_function_def(&mut self, func: ast::StmtAsyncFunctionDef) {
        let func_def = ast::StmtFunctionDef {
            name: func.name,
            args: func.args,
            body: func.body,
            decorator_list: func.decorator_list,
            returns: func.returns,
            type_comment: func.type_comment,
            type_params: func.type_params,
            range: func.range,
        };
        self.visit_function_def(func_def);
    }

    fn visit_class_def(&mut self, cls: ast::StmtClassDef) {
        let start = cls.range.start();
        let (line, col) = self.offset_to_line_col(start);
        let class_name = cls.name.to_string();
        
        // Only check for duplicate classes at the same scope level
        if !self.in_class && !self.in_function {
            // Top-level class
            if let Some(prev_loc) = self.defined_names.get(&class_name) {
                self.add_issue(
                    &crate::errors::E0102,
                    line,
                    col,
                    vec![class_name.clone(), format!("{}", prev_loc.0)],
                );
            } else {
                self.defined_names.insert(class_name.clone(), (line, col));
            }
        } else if self.in_class {
            // Nested class within another class - track separately
            if let Some(parent_class) = &self.current_class {
                let qualified_name = format!("{}::{}", parent_class, class_name);
                if let Some(prev_loc) = self.class_methods.get(&qualified_name) {
                    self.add_issue(
                        &crate::errors::E0102,
                        line,
                        col,
                        vec![class_name.clone(), format!("{}", prev_loc.0)],
                    );
                } else {
                    // Store nested class in class_methods map with qualified name
                    self.class_methods.insert(qualified_name, (line, col));
                }
            }
        }
        // If we're inside a function, nested classes are allowed

        let prev_in_class = self.in_class;
        let prev_class = self.current_class.clone();
        self.in_class = true;
        self.current_class = Some(class_name);

        for stmt in cls.body {
            self.visit_stmt(stmt);
        }

        self.in_class = prev_in_class;
        self.current_class = prev_class;
    }

    fn visit_return(&mut self, ret: ast::StmtReturn) {
        let start = ret.range.start();
        let (line, col) = self.offset_to_line_col(start);
        
        if !self.in_function {
            self.add_issue(&crate::errors::E0104, line, col, vec![]);
        } else if false && self.in_init && !self.in_generator && ret.value.is_some() {
            // E0101 is now reported at function level, not here
            // self.add_issue(&crate::errors::E0101, line, col, vec![]);
        } else if false {  // E0106 disabled - Pylint doesn't detect this
            // self.add_issue(&crate::errors::E0106, line, col, vec![]);
        }

        if let Some(value) = ret.value {
            self.visit_expr(*value);
        }
    }

    fn visit_continue(&mut self, cont: ast::StmtContinue) {
        if self.in_loop == 0 {
            let start = cont.range.start();
            let (line, col) = self.offset_to_line_col(start);
            self.add_issue(&crate::errors::E0103, line, col, vec!["'continue'".to_string()]);
        }
    }

    fn visit_break(&mut self, brk: ast::StmtBreak) {
        if self.in_loop == 0 {
            let start = brk.range.start();
            let (line, col) = self.offset_to_line_col(start);
            self.add_issue(&crate::errors::E0103, line, col, vec!["'break'".to_string()]);
        }
    }

    fn visit_for(&mut self, for_stmt: ast::StmtFor) {
        self.in_loop += 1;
        self.visit_expr(*for_stmt.iter);
        
        // Track the loop variable as definitely defined within the loop
        self.track_assignments(&for_stmt.target);
        
        for stmt in for_stmt.body {
            self.visit_stmt(stmt);
        }
        for stmt in for_stmt.orelse {
            self.visit_stmt(stmt);
        }
        self.in_loop -= 1;
    }

    fn visit_async_for(&mut self, for_stmt: ast::StmtAsyncFor) {
        self.in_loop += 1;
        self.visit_expr(*for_stmt.iter);
        
        // Track the loop variable as definitely defined within the loop
        self.track_assignments(&for_stmt.target);
        
        for stmt in for_stmt.body {
            self.visit_stmt(stmt);
        }
        for stmt in for_stmt.orelse {
            self.visit_stmt(stmt);
        }
        self.in_loop -= 1;
    }

    fn visit_while(&mut self, while_stmt: ast::StmtWhile) {
        self.in_loop += 1;
        self.visit_expr(*while_stmt.test);
        for stmt in while_stmt.body {
            self.visit_stmt(stmt);
        }
        for stmt in while_stmt.orelse {
            self.visit_stmt(stmt);
        }
        self.in_loop -= 1;
    }

    fn visit_if(&mut self, if_stmt: ast::StmtIf) {
        self.visit_expr(*if_stmt.test.clone());
        
        // Track variables defined in if branch
        let before_if = self.definitely_defined.clone();
        
        // For if/elif/else chains, check if the final else (not elif) terminates
        // Do this before consuming if_stmt
        let final_else_terminates = self.get_final_else_terminates(&if_stmt);
        let has_else = !if_stmt.orelse.is_empty();
        
        for stmt in if_stmt.body {
            self.visit_stmt(stmt);
        }
        
        let if_defined = self.definitely_defined.clone();
        
        // Reset to before if for else branch
        self.definitely_defined = before_if.clone();
        
        for stmt in if_stmt.orelse {
            self.visit_stmt(stmt);
        }
        
        let else_defined = self.definitely_defined.clone();
        
        // Variables are definitely defined only if defined in ALL branches
        if has_else {
            if final_else_terminates {
                // If the final else terminates (in if/else or if/elif/else),
                // variables from the if/elif branches are definitely defined
                // We need to properly collect all variables from if/elif branches
                self.definitely_defined = if_defined.union(&else_defined).cloned().collect();
                // Remove conditionally defined status for these variables
                for var in &self.definitely_defined {
                    self.conditionally_defined.remove(var);
                }
            } else {
                // Normal case: intersect definitions from all branches
                self.definitely_defined = if_defined.intersection(&else_defined).cloned().collect();
                
                // IMPORTANT: Variables that were defined before the if statement
                // and are reassigned in branches remain definitely defined
                // BUT only if we're in a function (not at module level)
                if self.in_function {
                    for var in &before_if {
                        // Only preserve if this variable is being reassigned in a branch
                        if if_defined.contains(var) || else_defined.contains(var) {
                            self.definitely_defined.insert(var.clone());
                            // Remove from conditionally defined since it's definitely defined
                            self.conditionally_defined.remove(var);
                        }
                    }
                }
            }
            
            // Mark variables as conditionally defined if not in all branches
            for var in if_defined.union(&else_defined) {
                if !self.definitely_defined.contains(var) {
                    self.conditionally_defined.insert(var.clone(), true);
                    // Only track as local variable if in a function
                    if self.in_function {
                        self.local_vars.insert(var.clone());
                    }
                    
                    // If we're at module level, also track in module_conditionally_defined
                    if !self.in_function && !self.in_class {
                        self.module_conditionally_defined.insert(var.clone());
                    }
                }
            }
        } else {
            // No else branch - variables from if are only conditionally defined
            self.definitely_defined = before_if;
            for var in if_defined {
                if !self.definitely_defined.contains(&var) {
                    self.conditionally_defined.insert(var.clone(), true);
                    // Only track as local variable if in a function
                    if self.in_function {
                        self.local_vars.insert(var.clone());
                    }
                    
                    // If we're at module level, also track in module_conditionally_defined
                    if !self.in_function && !self.in_class {
                        self.module_conditionally_defined.insert(var);
                    }
                }
            }
        }
    }

    fn visit_with(&mut self, with_stmt: ast::StmtWith) {
        for item in with_stmt.items {
            self.visit_expr(item.context_expr.clone());
        }
        for stmt in with_stmt.body {
            self.visit_stmt(stmt);
        }
    }

    fn visit_async_with(&mut self, with_stmt: ast::StmtAsyncWith) {
        for item in with_stmt.items {
            self.visit_expr(item.context_expr.clone());
        }
        for stmt in with_stmt.body {
            self.visit_stmt(stmt);
        }
    }

    fn visit_try(&mut self, try_stmt: ast::StmtTry) {
        // Save state before try block
        let before_try = self.definitely_defined.clone();
        let before_conditionally = self.conditionally_defined.clone();
        
        // Visit try body
        for stmt in try_stmt.body {
            self.visit_stmt(stmt);
        }
        
        let try_defined = self.definitely_defined.clone();
        let try_conditionally = self.conditionally_defined.clone();
        
        // Save the state after try block for the else clause
        let after_try_state = self.definitely_defined.clone();
        
        // Check if all except handlers terminate (return, raise, etc.)
        // If they do, variables defined in try block are definitely defined after try/except
        let all_handlers_terminate = !try_stmt.handlers.is_empty() && try_stmt.handlers.iter().all(|handler| {
            match handler {
                ast::ExceptHandler::ExceptHandler(h) => {
                    self.handler_always_terminates(&h.body)
                }
            }
        });
        
        // Special case: try/finally with no except handlers
        let no_except_handlers = try_stmt.handlers.is_empty();
        
        if all_handlers_terminate || no_except_handlers {
            // If all except handlers terminate OR there are no except handlers (try/finally),
            // variables from try are definitely defined after the block
            // (since if an exception occurs, we either handle and terminate, or propagate)
            self.definitely_defined = try_defined.clone();
        } else {
            // Variables defined in try are only conditionally defined
            // because an exception might occur before assignment
            self.definitely_defined = before_try.clone();
            
            // Track all variables that were defined (definitely or conditionally) in the try block
            for var in &try_defined {
                if !self.definitely_defined.contains(var) {
                    self.conditionally_defined.insert(var.clone(), true);
                    // Also track as local variable if in a function
                    if self.in_function {
                        self.local_vars.insert(var.clone());
                    }
                }
            }
            
            // Also mark variables that became conditionally defined within try block
            for (var, _) in &try_conditionally {
                if !before_conditionally.contains_key(var) && !self.definitely_defined.contains(var) {
                    self.conditionally_defined.insert(var.clone(), true);
                    if self.in_function {
                        self.local_vars.insert(var.clone());
                    }
                }
            }
        }
        
        // Visit except handlers and collect variables defined in ALL handlers
        let mut all_handlers_define_same: Option<HashSet<String>> = None;
        
        for handler in try_stmt.handlers {
            let before_handler = self.definitely_defined.clone();
            
            match handler {
                ast::ExceptHandler::ExceptHandler(h) => {
                    // Reset to state before try for this handler
                    self.definitely_defined = before_try.clone();
                    
                    for stmt in h.body {
                        self.visit_stmt(stmt);
                    }
                    
                    let handler_defined = self.definitely_defined.clone();
                    
                    // Track which variables are defined in ALL handlers
                    if let Some(ref mut common_vars) = all_handlers_define_same {
                        // Intersect with previous handlers
                        *common_vars = common_vars.intersection(&handler_defined).cloned().collect();
                    } else {
                        // First handler
                        all_handlers_define_same = Some(handler_defined);
                    }
                }
            }
            
            // Reset for next handler
            self.definitely_defined = before_handler;
        }
        
        // If there are handlers and they all define certain variables,
        // AND those same variables are defined in the try block,
        // then those variables are definitely defined after try/except
        if let Some(handler_common_vars) = all_handlers_define_same {
            if !all_handlers_terminate {
                // Variables defined in both try AND all except handlers are definitely defined
                let definitely_defined_after: HashSet<String> = try_defined.intersection(&handler_common_vars).cloned().collect();
                for var in definitely_defined_after {
                    self.definitely_defined.insert(var.clone());
                    self.conditionally_defined.remove(&var);
                }
            }
        }
        
        // Visit else clause (executed if no exception)
        // The else block runs only if the try block succeeded, so variables
        // defined in the try block are available
        if !try_stmt.orelse.is_empty() {
            // Restore state from after try block for the else clause
            self.definitely_defined = after_try_state;
            for stmt in try_stmt.orelse {
                self.visit_stmt(stmt);
            }
            // After else, merge the states
            self.definitely_defined = before_try.clone();
            // Variables from try are still only conditionally defined overall
            for var in &try_defined {
                if !self.definitely_defined.contains(var) {
                    self.conditionally_defined.insert(var.clone(), true);
                }
            }
        }
        
        // Visit finally clause
        for stmt in try_stmt.finalbody {
            self.visit_stmt(stmt);
        }
    }

    fn visit_global(&mut self, global_stmt: ast::StmtGlobal) {
        let start = global_stmt.range.start();
        let (line, col) = self.offset_to_line_col(start);
        
        for name in &global_stmt.names {
            let name_str = name.to_string();
            
            // E0115: Name is both nonlocal and global
            if self.nonlocal_names.contains(&name_str) {
                self.add_issue(&crate::errors::E0115, line, col, vec![name_str.clone()]);
            }
            
            self.global_names.insert(name_str);
        }
    }

    fn visit_nonlocal(&mut self, nonlocal_stmt: ast::StmtNonlocal) {
        let start = nonlocal_stmt.range.start();
        let (line, col) = self.offset_to_line_col(start);
        
        for name in &nonlocal_stmt.names {
            let name_str = name.to_string();
            
            // E0115: Name is both nonlocal and global
            if self.global_names.contains(&name_str) {
                self.add_issue(&crate::errors::E0115, line, col, vec![name_str.clone()]);
            }
            
            // E0117: nonlocal without binding
            if !self.in_function {
                self.add_issue(&crate::errors::E0117, line, col, vec![name_str.clone()]);
            }
            
            self.nonlocal_names.insert(name_str);
        }
    }

    fn visit_expr_stmt(&mut self, expr_stmt: ast::StmtExpr) {
        self.visit_expr(*expr_stmt.value);
    }
    
    fn visit_assign(&mut self, assign: ast::StmtAssign) {
        // Check for too many star expressions (E0112)
        let mut star_count = 0;
        for target in &assign.targets {
            star_count += self.count_starred_exprs(target);
        }
        if star_count > 1 {
            let start = assign.range.start();
            let (line, col) = self.offset_to_line_col(start);
            self.add_issue(&crate::errors::E0112, line, col, vec![]);
        }
        
        // Check for invalid star assignment (E0113)
        for target in &assign.targets {
            if let ast::Expr::Starred(_) = target {
                let start = assign.range.start();
                let (line, col) = self.offset_to_line_col(start);
                self.add_issue(&crate::errors::E0113, line, col, vec![]);
            }
            
            // Track variable definitions
            self.track_assignments(target);
        }
        
        // Visit the value being assigned
        self.visit_expr(*assign.value);
    }
    
    fn track_assignments(&mut self, expr: &ast::Expr) {
        match expr {
            ast::Expr::Name(name) => {
                let var_name = name.id.to_string();
                if self.in_function {
                    self.local_vars.insert(var_name.clone());
                }
                self.definitely_defined.insert(var_name.clone());
                self.conditionally_defined.remove(&var_name);
            }
            ast::Expr::Tuple(tuple) => {
                for elt in &tuple.elts {
                    self.track_assignments(elt);
                }
            }
            ast::Expr::List(list) => {
                for elt in &list.elts {
                    self.track_assignments(elt);
                }
            }
            _ => {}
        }
    }
    
    fn visit_ann_assign(&mut self, ann_assign: ast::StmtAnnAssign) {
        // Handle annotated assignments - these are variable definitions!
        if let Some(value) = ann_assign.value {
            // Visit the value expression first
            self.visit_expr(*value);
            
            // Track the assignment
            self.track_assignments(&ann_assign.target);
        }
        // Note: If there's no value, it's just a type annotation, not an assignment
    }
    
    fn handler_always_terminates(&self, body: &[ast::Stmt]) -> bool {
        // Check if the handler body always terminates (returns, raises, continues, or breaks)
        // This follows pylint's logic for determining if a code path terminates
        for stmt in body {
            match stmt {
                ast::Stmt::Return(_) | ast::Stmt::Raise(_) | ast::Stmt::Continue(_) | ast::Stmt::Break(_) => {
                    return true;
                }
                ast::Stmt::If(if_stmt) => {
                    // Check if both branches terminate
                    let if_terminates = self.handler_always_terminates(&if_stmt.body);
                    let else_terminates = !if_stmt.orelse.is_empty() && 
                                          self.handler_always_terminates(&if_stmt.orelse);
                    if if_terminates && else_terminates {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }
    
    fn get_final_else_terminates(&self, if_stmt: &ast::StmtIf) -> bool {
        // For if/elif/else chains, check if the final else (not elif) terminates
        if if_stmt.orelse.is_empty() {
            return false;
        }
        
        // Check if this is an elif (else contains only another if statement)
        if if_stmt.orelse.len() == 1 {
            if let ast::Stmt::If(nested_if) = &if_stmt.orelse[0] {
                // Recursively check the nested if/elif chain
                return self.get_final_else_terminates(nested_if);
            }
        }
        
        // This is a final else block (not elif)
        self.handler_always_terminates(&if_stmt.orelse)
    }
    
    fn count_starred_exprs(&self, expr: &ast::Expr) -> usize {
        match expr {
            ast::Expr::Starred(_) => 1,
            ast::Expr::Tuple(tuple) => {
                tuple.elts.iter().map(|e| self.count_starred_exprs(e)).sum()
            }
            ast::Expr::List(list) => {
                list.elts.iter().map(|e| self.count_starred_exprs(e)).sum()
            }
            _ => 0,
        }
    }

    fn visit_expr(&mut self, expr: ast::Expr) {
        use ast::Expr::*;
        
        match &expr {
            Name(name) => {
                let var_name = name.id.to_string();
                
                // Check for E0606: possibly used before assignment
                // We need to be careful to avoid false positives while catching real issues
                
                // First check if this is a local variable that's definitely defined
                let is_definitely_defined = self.definitely_defined.contains(&var_name) ||
                                          self.function_args.contains(&var_name) ||
                                          self.global_names.contains(&var_name) ||
                                          self.nonlocal_names.contains(&var_name);
                
                if !is_definitely_defined {
                    // Check two cases:
                    // 1. Module-level variable used in function that was conditionally defined at module level
                    // 2. Local variable that is conditionally defined within the function
                    
                    if self.in_function {
                        // Inside a function - check two sub-cases:
                        // a) Using a module-level conditional variable (_SIGN_KEY_ID case)
                        if self.module_conditionally_defined.contains(&var_name) &&
                           !self.local_vars.contains(&var_name) {
                            // This is a module-level variable that was conditionally defined
                            // being used inside a function - this is the _SIGN_KEY_ID case
                            let start = match &expr {
                                Name(n) => n.range.start(),
                                _ => return,
                            };
                            let (line, col) = self.offset_to_line_col(start);
                            self.add_issue(&crate::errors::E0606, line, col, vec![var_name.clone()]);
                        }
                        // b) Using a locally conditionally defined variable (bad_var_test case)
                        else if self.conditionally_defined.contains_key(&var_name) &&
                                self.local_vars.contains(&var_name) {
                            // This is a local variable that is conditionally defined within the function
                            // e.g., defined in a try block or if statement
                            let start = match &expr {
                                Name(n) => n.range.start(),
                                _ => return,
                            };
                            let (line, col) = self.offset_to_line_col(start);
                            self.add_issue(&crate::errors::E0606, line, col, vec![var_name.clone()]);
                        }
                    } else {
                        // At module level - check if using a conditionally defined variable
                        if self.conditionally_defined.contains_key(&var_name) {
                            let start = match &expr {
                                Name(n) => n.range.start(),
                                _ => return,
                            };
                            let (line, col) = self.offset_to_line_col(start);
                            self.add_issue(&crate::errors::E0606, line, col, vec![var_name.clone()]);
                        }
                    }
                }
            }
            Yield(yield_expr) => {
                if !self.in_function {
                    let start = yield_expr.range.start();
                    let (line, col) = self.offset_to_line_col(start);
                    self.add_issue(&crate::errors::E0105, line, col, vec![]);
                }
            }
            YieldFrom(yield_from) => {
                if !self.in_function {
                    let start = yield_from.range.start();
                    let (line, col) = self.offset_to_line_col(start);
                    self.add_issue(&crate::errors::E0105, line, col, vec![]);
                }
            }
            Call(call) => {
                // Check function call arguments
                self.check_function_call_args(&call);
                
                // Visit the function expression itself
                self.visit_expr(*call.func.clone());
                
                // Visit each argument (which may contain nested calls)
                for arg in &call.args {
                    self.visit_expr(arg.clone());
                }
                
                // Also visit keyword arguments
                for keyword in &call.keywords {
                    self.visit_expr(keyword.value.clone());
                }
            }
            ast::Expr::Tuple(tuple) => {
                // Visit each element in the tuple
                for elt in &tuple.elts {
                    self.visit_expr(elt.clone());
                }
            }
            ast::Expr::List(list) => {
                // Visit each element in the list
                for elt in &list.elts {
                    self.visit_expr(elt.clone());
                }
            }
            ast::Expr::Dict(dict) => {
                // Visit keys and values in the dict
                for key in &dict.keys {
                    if let Some(k) = key {
                        self.visit_expr(k.clone());
                    }
                }
                for value in &dict.values {
                    self.visit_expr(value.clone());
                }
            }
            _ => {}
        }
    }

    fn check_for_yield(&self, body: &[ast::Stmt]) -> bool {
        for stmt in body {
            if self.stmt_contains_yield(stmt) {
                return true;
            }
        }
        false
    }

    fn stmt_contains_yield(&self, stmt: &ast::Stmt) -> bool {
        use ast::Stmt::*;
        
        match stmt {
            Expr(expr_stmt) => self.expr_contains_yield(&expr_stmt.value),
            _ => false,
        }
    }

    fn expr_contains_yield(&self, expr: &ast::Expr) -> bool {
        use ast::Expr::*;
        
        matches!(expr, Yield(_) | YieldFrom(_))
    }

    fn check_for_return_value(&self, body: &[ast::Stmt]) -> bool {
        for stmt in body {
            if self.stmt_has_return_value(stmt) {
                return true;
            }
        }
        false
    }

    fn stmt_has_return_value(&self, stmt: &ast::Stmt) -> bool {
        use ast::Stmt::*;
        
        match stmt {
            Return(ret_stmt) => ret_stmt.value.is_some(),
            _ => false,
        }
    }

    fn visit_import(&mut self, import: ast::StmtImport) {
        for alias in &import.names {
            let module_name = alias.name.to_string();
            let import_name = alias.asname.as_ref()
                .map(|n| n.to_string())
                .unwrap_or_else(|| module_name.clone());
            
            self.imports.insert(import_name, module_name);
        }
    }
    
    fn visit_import_from(&mut self, import: ast::StmtImportFrom) {
        if let Some(module) = &import.module {
            let module_name = module.to_string();
            
            for alias in &import.names {
                let imported_name = alias.name.to_string();
                let local_name = alias.asname.as_ref()
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| imported_name.clone());
                
                // Store the full module path for this import
                self.imports.insert(local_name.clone(), format!("{}.{}", module_name, imported_name));
                
                // Now try to load the module and get function signatures
                self.load_module_signatures(&module_name, &imported_name, &local_name);
            }
        }
    }
    
    fn load_module_signatures(&mut self, module_name: &str, imported_name: &str, local_name: &str) {
        // Try to find and parse the module file
        let possible_paths = self.get_module_paths(module_name);
        
        for path in possible_paths {
            if path.exists() {
                if let Ok(source) = std::fs::read_to_string(&path) {
                    // Parse the module to extract function signatures
                    if let Ok(module_ast) = rustpython_parser::parse(&source, rustpython_parser::Mode::Module, "<module>") {
                        self.extract_function_signature_from_module(module_ast, imported_name, local_name);
                    }
                }
                break; // Found and processed the module
            }
        }
    }
    
    fn get_module_paths(&self, module_name: &str) -> Vec<std::path::PathBuf> {
        let mut paths = Vec::new();
        
        // Convert module name to path (e.g., "app.email_utils" -> "app/email_utils.py")
        let module_path = module_name.replace('.', "/");
        
        // Try relative to current file's directory
        if let Some(parent) = self.file_path.parent() {
            paths.push(parent.join(format!("{}.py", module_path)));
            
            // Also try from parent directory (common for imports like app.xxx when in app/)
            if let Some(grandparent) = parent.parent() {
                paths.push(grandparent.join(format!("{}.py", module_path)));
            }
        }
        
        // Try from working directory
        let cwd = std::env::current_dir().unwrap_or_default();
        paths.push(cwd.join(format!("{}.py", module_path)));
        
        
        paths
    }
    
    fn extract_function_signature_from_module(&mut self, module: ast::Mod, imported_name: &str, local_name: &str) {
        use crate::checkers::call_errors::FunctionSignature;
        
        if let ast::Mod::Module(ast::ModModule { body, .. }) = module {
            for stmt in body {
                if let ast::Stmt::FunctionDef(func) = stmt {
                    if func.name.as_str() == imported_name {
                        // Found the function we're importing
                        let mut required_args = Vec::new();
                        let mut min_args = 0;
                        
                        // Count required positional arguments
                        for arg in &func.args.posonlyargs {
                            if arg.default.is_none() {
                                let arg_name = arg.def.arg.to_string();
                                required_args.push(arg_name);
                                min_args += 1;
                            }
                        }
                        for arg in &func.args.args {
                            if arg.default.is_none() {
                                let arg_name = arg.def.arg.to_string();
                                required_args.push(arg_name);
                                min_args += 1;
                            }
                        }
                        
                        let max_args = if func.args.vararg.is_some() {
                            None
                        } else {
                            Some(func.args.posonlyargs.len() + func.args.args.len() + func.args.kwonlyargs.len())
                        };
                        
                        let signature = FunctionSignature {
                            name: local_name.to_string(),
                            min_args,
                            max_args,
                            required_args,
                            has_varargs: func.args.vararg.is_some(),
                            has_kwargs: func.args.kwarg.is_some(),
                        };
                        self.function_signatures.insert(local_name.to_string(), signature);
                        break;
                    }
                }
            }
        }
    }

    fn is_decorator_name(&self, decorator: &ast::Expr, name: &str) -> bool {
        match decorator {
            ast::Expr::Name(n) => n.id.as_str() == name,
            ast::Expr::Attribute(attr) => {
                // Handle decorators like builtins.staticmethod
                attr.attr.as_str() == name
            }
            ast::Expr::Call(call) => {
                // Handle decorators with arguments like @decorator()
                self.is_decorator_name(&call.func, name)
            }
            _ => false,
        }
    }

    pub fn offset_to_line_col(&self, offset: TextSize) -> (usize, usize) {
        let offset = offset.to_usize();
        let mut line = 1;
        let mut col = 1;
        
        for (i, ch) in self.source.char_indices() {
            if i >= offset {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        
        (line, col)
    }
}