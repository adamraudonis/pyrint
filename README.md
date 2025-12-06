# Prylint

A blazing-fast Python linter written in Rust, providing 50-80x faster analysis compared to traditional Python linters.

## Features

- ⚡ **Lightning Fast**: 50-80x faster than Pylint on real codebases
- 🎯 **High Accuracy**: 97.8% accuracy on implemented error codes
- 🦀 **Rust-Powered**: Built with Rust for maximum performance
- 📦 **Easy Installation**: Simple pip install with no complex dependencies
- 🔍 **Growing Coverage**: Actively expanding error detection capabilities

## Installation

```bash
pip install prylint
```

## Usage

### Command Line

```bash
# Lint a single file
prylint script.py

# Lint a directory
prylint src/

# Output as JSON
prylint --json script.py

# Lint without recursion
prylint --no-recursive src/
```

### Python API

```python
from prylint import lint_file, lint_directory

# Lint a single file
issues = lint_file("script.py")
for issue in issues:
    print(f"{issue.file}:{issue.line}: {issue.code} {issue.message}")

# Lint a directory
issues = lint_directory("src/", recursive=True)
```

## Supported Error Codes

Prylint currently implements 35 error codes from the Pylint error code set:

### Fully Implemented

- **E0100**: `__init__` method is a generator
- **E0101**: Explicit return in `__init__`
- **E0102**: Function/method redefined
- **E0103**: break/continue not in loop
- **E0104**: Return outside function
- **E0105**: Yield outside function
- **E0106**: Return with argument in generator
- **E0108**: Duplicate argument names
- **E0109**: Duplicate dictionary keys
- **E0112**: Too many starred expressions
- **E0115**: Name is nonlocal and global
- **E0116**: Continue not in loop
- **E0117**: Nonlocal without binding
- **E0118**: Used before global declaration
- **E0606**: Possibly used before assignment
- **E0704**: Misplaced bare raise
- **E0711**: NotImplemented raised
- **E1142**: await outside async

## Performance

Benchmark results on real Python codebases:

| Linter  | Time  | Speedup |
| ------- | ----- | ------- |
| Pylint  | 11.2s | 1x      |
| Prylint | 0.22s | 50x     |

## Why Prylint?

Prylint is designed for developers who want:

- Fast feedback during development
- Integration with CI/CD pipelines without slowing builds
- A modern, performant alternative to traditional linters
- Focus on catching real errors, not style issues

## Development

### Building from Source

```bash
# Clone the repository
git clone https://github.com/adamraudonis/prylint.git
cd prylint

# Build the Rust binary (debug mode)
cargo build

# Build the Rust binary (release mode)
cargo build --release

# Run tests
cargo test
python tests/run_error_code_tests.py
```

### Creating a Release

```bash
# 1. Update version in Cargo.toml and pyproject.toml
# Edit both files to bump version (e.g., 0.1.0 -> 0.1.1)

# 2. Build release binary
cargo build --release

# 3. Build Python distribution
python setup_simple.py sdist bdist_wheel

# 4. Upload to PyPI
python -m twine upload dist/prylint-VERSION*

# Note: The project uses setup_simple.py which includes the pre-built
# Rust binary in the Python package, avoiding build-time Rust compilation
```

## Contributing

Contributions are welcome! Please feel free to submit issues and pull requests on [GitHub](https://github.com/adamraudonis/prylint).

## Credits

Built with ❤️ using:

- [RustPython Parser](https://github.com/RustPython/RustPython) for Python AST parsing
- [PyO3](https://github.com/PyO3/pyo3) for Python bindings

## Goal

Goal is to be equivalent to running:

```
pylint . -E --disable=C0301,W0511,C0114,R0402,C0116,R0914,W0718,R1735,W0105,R1705,W0603,W0104,C0209,W0719,C0411,C0412,R0912,R0915,C0413,C0115,C0103,W0613,R0801,W0602,R0913,R0917,W0622,R0902,R0911,R0913,R1702,R1716,W0212,R1728,C0121,R0916,C0415,W1401,C0206,C0302,R0904,W1514,R0903,E0110,R1714,W0707,R1718,W1309,W1203,E0611,W0611,W0108,W0177,E1101,C1803,R1721,W0123,R1720,R1710,W0221,W0122,C0201,W1510,R1729,R1737,C0325,R0401,E0401
```
