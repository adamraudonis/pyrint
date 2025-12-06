#!/usr/bin/env python3
"""Command-line interface for Prylint."""

import sys
import argparse
import json
from pathlib import Path
from .linter import lint_file, lint_directory, PrylintError


def main():
    """Main entry point for the prylint CLI."""
    parser = argparse.ArgumentParser(
        description="Prylint - A fast Python linter written in Rust",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  prylint file.py                  # Lint a single file
  prylint src/                     # Lint all Python files in directory
  prylint --json file.py           # Output as JSON
  prylint --no-recursive src/      # Don't recurse into subdirectories
        """
    )
    
    parser.add_argument(
        "path",
        help="Path to Python file or directory to lint"
    )
    
    parser.add_argument(
        "--json",
        action="store_true",
        help="Output results as JSON"
    )
    
    parser.add_argument(
        "--no-recursive",
        action="store_true",
        help="Don't recursively lint subdirectories"
    )
    
    parser.add_argument(
        "--quiet", "-q",
        action="store_true",
        help="Only show error count"
    )
    
    parser.add_argument(
        "-E", "--errors-only",
        action="store_true",
        help="Display only error messages (ignore warnings)"
    )
    
    parser.add_argument(
        "--disable",
        help="Disable specific checkers (comma-separated error codes, e.g., E0401,W0611)"
    )
    
    parser.add_argument(
        "--enable",
        help="Enable specific checkers (comma-separated error codes)"
    )
    
    parser.add_argument(
        "--version", "-v",
        action="version",
        version="%(prog)s 0.1.0"
    )
    
    args = parser.parse_args()
    
    try:
        path = Path(args.path)
        
        if path.is_file():
            issues = lint_file(
                str(path), 
                json_output=False,
                errors_only=args.errors_only,
                disable=args.disable,
                enable=args.enable
            )
        elif path.is_dir():
            issues = lint_directory(
                str(path),
                recursive=not args.no_recursive,
                json_output=False,
                errors_only=args.errors_only,
                disable=args.disable,
                enable=args.enable
            )
        else:
            print(f"Error: {path} is not a valid file or directory", file=sys.stderr)
            return 1
        
        if args.json:
            # Output as JSON
            output = {
                "issues": [issue.to_dict() for issue in issues],
                "summary": {
                    "total": len(issues),
                    "errors": sum(1 for i in issues if i.severity == "error"),
                    "warnings": sum(1 for i in issues if i.severity == "warning")
                }
            }
            print(json.dumps(output, indent=2))
        elif args.quiet:
            # Just show count
            print(f"{len(issues)} issue(s) found")
        else:
            # Standard output
            if issues:
                for issue in issues:
                    print(f"{issue.file}:{issue.line}:{issue.column}: {issue.code}: {issue.message} ({issue.symbol})")
                print(f"\nSummary: {len(issues)} issue(s) found")
            else:
                print("No issues found!")
        
        return 1 if issues else 0
        
    except PrylintError as e:
        print(f"Error: {e}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("\nInterrupted", file=sys.stderr)
        return 130
    except Exception as e:
        print(f"Unexpected error: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())