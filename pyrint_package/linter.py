"""Main linting interface for Pyrint."""

import json
import subprocess
import sys
import os
from pathlib import Path
from typing import List, Dict, Any, Optional


class PyrintError(Exception):
    """Base exception for Pyrint errors."""
    pass


class Issue:
    """Represents a linting issue found by Pyrint."""
    
    def __init__(self, code: str, message: str, file: str, line: int, column: int, severity: str, symbol: str):
        self.code = code
        self.message = message
        self.file = file
        self.line = line
        self.column = column
        self.severity = severity
        self.symbol = symbol
    
    def __repr__(self):
        return f"Issue({self.code}: {self.message} at {self.file}:{self.line}:{self.column})"
    
    def to_dict(self) -> Dict[str, Any]:
        return {
            "code": self.code,
            "message": self.message,
            "file": self.file,
            "line": self.line,
            "column": self.column,
            "severity": self.severity,
            "symbol": self.symbol
        }


def _find_pyrint_binary() -> str:
    """Find the pyrint binary in the package."""
    # Look for the Rust binary in common locations
    possible_paths = [
        # Installed via pip - binary should be in package directory
        Path(__file__).parent / "bin" / "pyrint",
        Path(__file__).parent / "pyrint",
        # Development mode
        Path(__file__).parent.parent / "target" / "release" / "pyrint",
        Path(__file__).parent.parent / "target" / "debug" / "pyrint",
        # System PATH
        "pyrint",
    ]
    
    for path in possible_paths:
        if isinstance(path, Path):
            if path.exists() and path.is_file():
                return str(path)
        else:
            # Try to find in PATH
            try:
                result = subprocess.run(["which", path], capture_output=True, text=True)
                if result.returncode == 0:
                    return path
            except:
                continue
    
    raise PyrintError(
        "Could not find pyrint binary. Please ensure the package was installed correctly."
    )


def lint_file(filepath: str, json_output: bool = True) -> List[Issue]:
    """
    Lint a single Python file.
    
    Args:
        filepath: Path to the Python file to lint
        json_output: Whether to parse JSON output (True) or return raw text
        
    Returns:
        List of Issue objects found in the file
        
    Raises:
        PyrintError: If linting fails or file doesn't exist
    """
    if not os.path.exists(filepath):
        raise PyrintError(f"File not found: {filepath}")
    
    binary = _find_pyrint_binary()
    
    cmd = [binary]
    if json_output:
        cmd.append("--json")
    cmd.append(filepath)
    
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, check=False)
        
        if result.returncode not in [0, 1]:  # 0 = no issues, 1 = issues found
            raise PyrintError(f"Pyrint failed: {result.stderr}")
        
        if json_output and result.stdout:
            try:
                data = json.loads(result.stdout)
                return [Issue(**issue) for issue in data.get("issues", [])]
            except json.JSONDecodeError:
                # Fall back to text parsing if JSON fails
                return _parse_text_output(result.stdout)
        else:
            return _parse_text_output(result.stdout)
            
    except FileNotFoundError:
        raise PyrintError(f"Could not execute pyrint binary at: {binary}")
    except Exception as e:
        raise PyrintError(f"Linting failed: {str(e)}")


def lint_directory(directory: str, recursive: bool = True, json_output: bool = True) -> List[Issue]:
    """
    Lint all Python files in a directory.
    
    Args:
        directory: Path to the directory to lint
        recursive: Whether to recursively lint subdirectories
        json_output: Whether to parse JSON output (True) or return raw text
        
    Returns:
        List of Issue objects found in all files
        
    Raises:
        PyrintError: If linting fails or directory doesn't exist
    """
    if not os.path.exists(directory):
        raise PyrintError(f"Directory not found: {directory}")
    
    if not os.path.isdir(directory):
        raise PyrintError(f"Not a directory: {directory}")
    
    all_issues = []
    
    if recursive:
        pattern = "**/*.py"
    else:
        pattern = "*.py"
    
    for py_file in Path(directory).glob(pattern):
        try:
            issues = lint_file(str(py_file), json_output=json_output)
            all_issues.extend(issues)
        except PyrintError:
            # Skip files that can't be linted
            continue
    
    return all_issues


def _parse_text_output(output: str) -> List[Issue]:
    """Parse text output from pyrint into Issue objects."""
    issues = []
    
    for line in output.split('\n'):
        if not line or line.startswith('*'):
            continue
        
        # Parse format: filepath:line:column: CODE: message (symbol)
        parts = line.split(':', 3)
        if len(parts) >= 4:
            try:
                filepath = parts[0]
                line_num = int(parts[1])
                column = int(parts[2])
                
                # Parse the rest: " CODE: message (symbol)"
                rest = parts[3].strip()
                if ' ' in rest:
                    code_part, msg_part = rest.split(' ', 1)
                    code = code_part.rstrip(':')
                    
                    # Extract symbol from message
                    symbol = ""
                    message = msg_part
                    if '(' in msg_part and msg_part.endswith(')'):
                        idx = msg_part.rfind('(')
                        message = msg_part[:idx].strip()
                        symbol = msg_part[idx+1:-1]
                    
                    # Determine severity from code
                    severity = "error" if code.startswith('E') else "warning"
                    
                    issues.append(Issue(
                        code=code,
                        message=message,
                        file=filepath,
                        line=line_num,
                        column=column,
                        severity=severity,
                        symbol=symbol
                    ))
            except (ValueError, IndexError):
                # Skip malformed lines
                continue
    
    return issues