use prylint::linter::Linter;
use prylint::config::Config;
use prylint::errors::{Issue, Severity};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn create_test_file(dir: &TempDir, filename: &str, content: &str) -> PathBuf {
    let file_path = dir.path().join(filename);
    fs::write(&file_path, content).unwrap();
    file_path
}

fn run_linter(content: &str) -> Vec<Issue> {
    let dir = TempDir::new().unwrap();
    let file_path = create_test_file(&dir, "test.py", content);
    
    let config = Config::default();
    let mut linter = Linter::new(config);
    
    linter.check_file(&file_path).unwrap_or_else(|_| Vec::new())
}

#[test]
fn test_e0001_syntax_error() {
    let code = r#"
def bad_function(
    print("missing closing paren"
"#;
    let issues = run_linter(code);
    assert!(issues.iter().any(|i| i.code == "E0001"));
}

#[test]
fn test_e0100_init_is_generator() {
    let code = r#"
class MyClass:
    def __init__(self):
        yield 1
"#;
    let issues = run_linter(code);
    assert!(issues.iter().any(|i| i.code == "E0100"));
}

#[test]
fn test_e0101_return_in_init() {
    let code = r#"
class MyClass:
    def __init__(self):
        return "value"
"#;
    let issues = run_linter(code);
    assert!(issues.iter().any(|i| i.code == "E0101"));
}

#[test]
fn test_e0102_function_redefined() {
    let code = r#"
def my_func():
    pass

def my_func():
    pass
"#;
    let issues = run_linter(code);
    assert!(issues.iter().any(|i| i.code == "E0102"));
}

#[test]
fn test_e0104_return_outside_function() {
    let code = "return 42";
    let issues = run_linter(code);
    assert!(issues.iter().any(|i| i.code == "E0104"));
}

#[test]
fn test_e0105_yield_outside_function() {
    let code = "yield 1";
    let issues = run_linter(code);
    assert!(issues.iter().any(|i| i.code == "E0105"));
}

#[test]
fn test_e0106_return_in_generator() {
    let code = r#"
def my_generator():
    yield 1
    return 2
"#;
    let issues = run_linter(code);
    assert!(issues.iter().any(|i| i.code == "E0106"));
}

#[test]
fn test_e0108_duplicate_argument() {
    let code = r#"
def func(arg1, arg1):
    pass
"#;
    let issues = run_linter(code);
    assert!(issues.iter().any(|i| i.code == "E0108"));
}

#[test]
fn test_e0116_continue_not_in_loop() {
    let code = r#"
def test():
    continue
"#;
    let issues = run_linter(code);
    assert!(issues.iter().any(|i| i.code == "E0116"));
}

#[test]
fn test_valid_code_no_errors() {
    let code = r#"
class MyClass:
    def __init__(self):
        self.value = 10
        
    def method(self):
        return self.value

def normal_function(arg1, arg2):
    for i in range(10):
        if i == 5:
            break
        if i == 3:
            continue
    return arg1 + arg2
"#;
    let issues = run_linter(code);
    let error_issues: Vec<_> = issues.iter().filter(|i| i.severity == Severity::Error).collect();
    assert_eq!(error_issues.len(), 0);
}

#[test]
fn test_e0115_nonlocal_and_global() {
    let code = r#"
def outer():
    x = 1
    def inner():
        global x
        nonlocal x
"#;
    let issues = run_linter(code);
    assert!(issues.iter().any(|i| i.code == "E0115"));
}

#[test]
fn test_e0117_nonlocal_without_binding() {
    let code = r#"
def func():
    nonlocal x
"#;
    let issues = run_linter(code);
    assert!(issues.iter().any(|i| i.code == "E0117"));
}