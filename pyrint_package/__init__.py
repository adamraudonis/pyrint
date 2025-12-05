"""
Pyrint - A fast Python linter written in Rust.

Pyrint provides blazing-fast Python code analysis with 50-80x speedup
compared to traditional Python linters.
"""

__version__ = "0.1.0"
__author__ = "Adam Raudonis"

from .linter import lint_file, lint_directory, PyrintError

__all__ = ["lint_file", "lint_directory", "PyrintError", "__version__"]