use criterion::{black_box, criterion_group, criterion_main, Criterion};
use prylint::config::Config;
use prylint::linter::Linter;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn create_large_python_file(lines: usize) -> String {
    let mut code = String::new();
    
    for i in 0..lines / 10 {
        code.push_str(&format!(
            r#"
def function_{}(arg1, arg2, arg3):
    result = arg1 + arg2
    for i in range(arg3):
        if i % 2 == 0:
            result += i
        else:
            result -= i
    return result

class Class_{}:
    def __init__(self):
        self.value = 0
    
    def method(self, x):
        return x * 2
"#,
            i, i
        ));
    }
    
    code
}

fn benchmark_linting(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    
    // Create test files of different sizes
    let small_code = create_large_python_file(100);
    let medium_code = create_large_python_file(1000);
    let large_code = create_large_python_file(10000);
    
    let small_file = dir.path().join("small.py");
    let medium_file = dir.path().join("medium.py");
    let large_file = dir.path().join("large.py");
    
    fs::write(&small_file, &small_code).unwrap();
    fs::write(&medium_file, &medium_code).unwrap();
    fs::write(&large_file, &large_code).unwrap();
    
    let config = Config::default();
    
    c.bench_function("lint_small_file", |b| {
        b.iter(|| {
            let mut linter = Linter::new(config.clone());
            linter.check_file(black_box(&small_file))
        });
    });
    
    c.bench_function("lint_medium_file", |b| {
        b.iter(|| {
            let mut linter = Linter::new(config.clone());
            linter.check_file(black_box(&medium_file))
        });
    });
    
    c.bench_function("lint_large_file", |b| {
        b.iter(|| {
            let mut linter = Linter::new(config.clone());
            linter.check_file(black_box(&large_file))
        });
    });
    
    // Benchmark parallel processing
    let mut parallel_config = config.clone();
    parallel_config.jobs = 4;
    
    c.bench_function("lint_directory_parallel", |b| {
        b.iter(|| {
            let mut linter = Linter::new(parallel_config.clone());
            linter.check_directory(black_box(dir.path()))
        });
    });
}

criterion_group!(benches, benchmark_linting);
criterion_main!(benches);