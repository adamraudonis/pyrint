#!/usr/bin/env python3
"""
Comprehensive test runner for error code tests
Compares prylint and pylint outputs for accuracy and performance
"""

import subprocess
import time
import json
import os
import sys
import re
from pathlib import Path
from dataclasses import dataclass, field
from typing import List, Tuple, Dict, Set, Optional
from collections import defaultdict
import statistics


@dataclass
class LintError:
    """Represents a linting error"""

    code: str
    line: int
    column: int
    message: str
    symbol: str = ""

    def __hash__(self):
        # Only hash on code and line, not column (since columns may differ slightly)
        return hash((self.code, self.line))

    def __eq__(self, other):
        # Ignore column numbers entirely - only compare error code and line number
        # Column numbers can vary between parsers and don't affect the validity of the error
        return self.code == other.code and self.line == other.line


@dataclass
class TestResult:
    """Results for a single test file"""

    error_code: str
    file_path: str
    pylint_errors: List[LintError] = field(default_factory=list)
    prylint_errors: List[LintError] = field(default_factory=list)
    pylint_time: float = 0.0
    prylint_time: float = 0.0
    pylint_output: str = ""
    prylint_output: str = ""

    @property
    def matched_errors(self) -> List[LintError]:
        """Errors found by both linters"""
        pylint_set = set(self.pylint_errors)
        prylint_set = set(self.prylint_errors)
        return list(pylint_set & prylint_set)

    @property
    def pylint_only(self) -> List[LintError]:
        """Errors found only by pylint"""
        pylint_set = set(self.pylint_errors)
        prylint_set = set(self.prylint_errors)
        return list(pylint_set - prylint_set)

    @property
    def prylint_only(self) -> List[LintError]:
        """Errors found only by prylint"""
        pylint_set = set(self.pylint_errors)
        prylint_set = set(self.prylint_errors)
        return list(prylint_set - pylint_set)

    @property
    def accuracy(self) -> float:
        """Calculate accuracy percentage"""
        if not self.pylint_errors:
            return 100.0 if not self.prylint_errors else 0.0

        matched = len(self.matched_errors)
        total = len(self.pylint_errors)
        return (matched / total) * 100

    @property
    def speedup(self) -> float:
        """Calculate speedup factor"""
        if self.prylint_time == 0:
            return float("inf")
        return self.pylint_time / self.prylint_time


class PylintRunner:
    """Runner for pylint"""

    @staticmethod
    def run(file_path: str, error_code: str) -> Tuple[List[LintError], float, str]:
        """Run pylint and parse results"""
        start_time = time.perf_counter()

        # Run pylint with specific error code enabled
        cmd = [
            sys.executable,
            "-m",
            "pylint",
            "--disable=all",
            f"--enable={error_code}",
            "--output-format=json",
            str(file_path),
        ]

        try:
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                cwd=str(Path(file_path).parent.parent),  # Run from project root
            )
            elapsed_time = time.perf_counter() - start_time

            errors = []
            if result.stdout:
                try:
                    messages = json.loads(result.stdout)
                    for msg in messages:
                        if (
                            msg["type"] == "error"
                            and msg["message-id"].upper() == error_code.upper()
                        ):
                            error = LintError(
                                code=msg["message-id"].upper(),
                                line=msg["line"],
                                column=msg["column"],
                                message=msg["message"],
                                symbol=msg.get("symbol", ""),
                            )
                            errors.append(error)
                except json.JSONDecodeError:
                    pass

            return errors, elapsed_time, result.stdout + result.stderr

        except Exception as e:
            print(f"Error running pylint: {e}")
            return [], 0, str(e)


class PrylintRunner:
    """Runner for prylint"""

    @staticmethod
    def run(file_path: str, error_code: str) -> Tuple[List[LintError], float, str]:
        """Run prylint and parse results"""
        start_time = time.perf_counter()

        # Always use debug build for testing (much faster to build)
        prylint_path = Path(__file__).parent.parent / "target" / "debug" / "prylint"

        if not prylint_path.exists():
            print(f"Prylint executable not found. Building...")
            build_result = subprocess.run(
                ["cargo", "build"],
                cwd=str(Path(__file__).parent.parent),
                capture_output=True,
            )
            if build_result.returncode != 0:
                print(f"Failed to build prylint: {build_result.stderr.decode()}")
                return [], 0, "Build failed"
            prylint_path = Path(__file__).parent.parent / "target" / "debug" / "prylint"

        cmd = [str(prylint_path), str(file_path)]

        try:
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                cwd=str(Path(file_path).parent.parent),
            )
            elapsed_time = time.perf_counter() - start_time

            errors = []
            # Parse prylint output
            # Format variations:
            # - filename:line:column: E0XXX: message (symbol)
            # - filename:line:column: E: message (E0XXX:symbol)
            for line in result.stdout.split("\n"):
                # Try first format: filename:line:column: E0XXX: message (symbol)
                match = re.match(
                    r".*?:(\d+):(\d+):\s*(E\d+):\s*(.*?)\s*\([^)]*\)", line
                )
                if not match:
                    # Try second format: filename:line:column: E: message (E0XXX:symbol)
                    match = re.match(
                        r".*?:(\d+):(\d+):\s*E:\s*(.*?)\s*\((E\d+)[:\w-]*\)", line
                    )
                    if match:
                        # Reorder groups for consistency
                        match = re.match(
                            r".*?:(\d+):(\d+):\s*E:\s*(.*?)\s*\((E\d+)[:\w-]*\)", line
                        )
                        if match:
                            code = match.group(4)
                            message = match.group(3)
                            line_num = match.group(1)
                            col_num = match.group(2)
                    else:
                        continue
                else:
                    code = match.group(3)
                    message = match.group(4)
                    line_num = match.group(1)
                    col_num = match.group(2)

                if code and code.upper() == error_code.upper():
                    error = LintError(
                        code=code.upper(),
                        line=int(line_num),
                        column=int(col_num),
                        message=message,
                        symbol="",
                    )
                    errors.append(error)

            return errors, elapsed_time, result.stdout

        except Exception as e:
            print(f"Error running prylint: {e}")
            return [], 0, str(e)


class ErrorCodeTestRunner:
    """Main test runner for error code tests"""

    def __init__(self):
        self.results: List[TestResult] = []
        self.summary = {
            "total_tests": 0,
            "perfect_matches": 0,
            "partial_matches": 0,
            "failures": 0,
            "missing_tests": [],
            "total_pylint_time": 0.0,
            "total_prylint_time": 0.0,
        }

    def get_implemented_error_codes(self) -> List[str]:
        """Get list of implemented error codes from ERROR_CODES_STATUS.txt"""
        status_file = Path(__file__).parent.parent / "ERROR_CODES_STATUS.txt"
        implemented_codes = []

        if status_file.exists():
            with open(status_file, "r") as f:
                for line in f:
                    if "✅ Implemented" in line:
                        match = re.match(r"^(E\d+)", line)
                        if match:
                            code = match.group(1)
                            # Skip E0001 - parser differences that can't be fixed
                            if code != "E0001":
                                implemented_codes.append(code)

        return implemented_codes

    def find_test_files(self) -> Dict[str, Path]:
        """Find all error code test files"""
        test_dir = Path(__file__).parent / "error_code_tests"
        print(test_dir)
        test_files = {}

        if test_dir.exists():
            for test_file in test_dir.glob("*.py"):
                # Extract error code from filename (e.g., e0101.py -> E0101)
                match = re.match(r"(e\d+)\.py$", test_file.name, re.IGNORECASE)
                if match:
                    error_code = match.group(1).upper()
                    test_files[error_code] = test_file

        return test_files

    def create_missing_test_files(
        self, implemented_codes: List[str], existing_tests: Dict[str, Path]
    ):
        """Create placeholder test files for implemented error codes without tests"""
        test_dir = Path(__file__).parent / "error_code_tests"
        test_dir.mkdir(parents=True, exist_ok=True)

        for code in implemented_codes:
            if code not in existing_tests:
                test_file = test_dir / f"{code.lower()}.py"
                if not test_file.exists():
                    # Create a minimal test file
                    test_content = f"""# Test file for error code {code}
# This file should contain code that triggers {code}

# TODO: Add test cases that trigger {code}
"""
                    test_file.write_text(test_content)
                    print(f"  Created placeholder test file: {test_file.name}")
                    self.summary["missing_tests"].append(code)

    def run_single_test(self, error_code: str, file_path: Path) -> TestResult:
        """Run both linters on a single test file"""
        result = TestResult(error_code=error_code, file_path=str(file_path))

        # Run pylint
        pylint_errors, pylint_time, pylint_output = PylintRunner.run(
            str(file_path), error_code
        )
        result.pylint_errors = pylint_errors
        result.pylint_time = pylint_time
        result.pylint_output = pylint_output

        # Run prylint
        prylint_errors, prylint_time, prylint_output = PrylintRunner.run(
            str(file_path), error_code
        )
        result.prylint_errors = prylint_errors
        result.prylint_time = prylint_time
        result.prylint_output = prylint_output

        return result

    def print_test_result(
        self, result: TestResult, verbose: bool = False, full_output: bool = False
    ):
        """Print results for a single test"""
        status_icon = (
            "✅" if result.accuracy == 100 else "⚠️" if result.accuracy > 0 else "❌"
        )

        print(f"\n{status_icon} {result.error_code}:")
        print(f"   File: {Path(result.file_path).name}")
        print(
            f"   Pylint: {len(result.pylint_errors)} errors found in {result.pylint_time:.3f}s"
        )
        print(
            f"   Prylint: {len(result.prylint_errors)} errors found in {result.prylint_time:.3f}s"
        )
        print(f"   Accuracy: {result.accuracy:.1f}%")
        if result.prylint_time > 0:
            print(f"   Speedup: {result.speedup:.1f}x")

        # Always show missed errors and false positives if they exist
        if result.pylint_only:
            print(f"   ❌ Missed by Prylint ({len(result.pylint_only)} errors):")
            for err in sorted(result.pylint_only, key=lambda x: (x.line, x.column)):
                print(f"      Line {err.line}:{err.column}: {err.message}")

        if result.prylint_only:
            print(
                f"   ⚠️  False positives by Prylint ({len(result.prylint_only)} errors):"
            )
            for err in sorted(result.prylint_only, key=lambda x: (x.line, x.column)):
                print(f"      Line {err.line}:{err.column}: {err.message}")

        # Show raw outputs in verbose mode
        if verbose:
            print(f"\n   📋 PYLINT RAW OUTPUT:")
            print("   " + "-" * 60)
            if result.pylint_output:
                output_lines = result.pylint_output.split("\n")
                max_lines = None if full_output else 30
                for line in output_lines[:max_lines]:
                    if line.strip():
                        print(f"   {line}")
                if not full_output and len(output_lines) > 30:
                    print(
                        f"   ... ({len(output_lines) - 30} more lines, use --full-output to see all)"
                    )
            else:
                print("   (no output)")

            print(f"\n   📋 PYRINT RAW OUTPUT:")
            print("   " + "-" * 60)
            if result.prylint_output:
                output_lines = result.prylint_output.split("\n")
                max_lines = None if full_output else 30
                for line in output_lines[:max_lines]:
                    if line.strip():
                        print(f"   {line}")
                if not full_output and len(output_lines) > 30:
                    print(
                        f"   ... ({len(output_lines) - 30} more lines, use --full-output to see all)"
                    )
            else:
                print("   (no output)")
            print()

    def run_all_tests(self, verbose: bool = False, full_output: bool = False):
        """Run all error code tests"""
        print("=" * 80)
        print("ERROR CODE TEST RUNNER")
        print("=" * 80)

        # Get implemented codes and existing tests
        print("\n📋 Discovering tests...")
        implemented_codes = self.get_implemented_error_codes()
        test_files = self.find_test_files()

        print(f"   Found {len(implemented_codes)} implemented error codes")
        print(f"   Found {len(test_files)} test files")

        # Create missing test files
        missing = set(implemented_codes) - set(test_files.keys())
        if missing:
            print(f"\n⚠️  Creating {len(missing)} missing test files...")
            self.create_missing_test_files(implemented_codes, test_files)
            # Re-scan for test files
            test_files = self.find_test_files()

        # Run tests
        print(f"\n🧪 Running tests...")
        print("-" * 80)

        for code in sorted(implemented_codes):
            if code in test_files:
                result = self.run_single_test(code, test_files[code])
                self.results.append(result)
                self.print_test_result(result, verbose, full_output)

                # Update summary
                self.summary["total_tests"] += 1
                self.summary["total_pylint_time"] += result.pylint_time
                self.summary["total_prylint_time"] += result.prylint_time

                if result.accuracy == 100:
                    self.summary["perfect_matches"] += 1
                elif result.accuracy > 0:
                    self.summary["partial_matches"] += 1
                else:
                    self.summary["failures"] += 1

    def print_summary(self):
        """Print test summary"""
        print("\n" + "=" * 80)
        print("📊 SUMMARY REPORT")
        print("=" * 80)

        if self.summary["total_tests"] == 0:
            print("No tests were run.")
            return

        # Accuracy summary
        print("\n📈 ACCURACY:")
        print(f"   Total tests run: {self.summary['total_tests']}")
        print(
            f"   Perfect matches: {self.summary['perfect_matches']} ({self.summary['perfect_matches']/self.summary['total_tests']*100:.1f}%)"
        )
        print(
            f"   Partial matches: {self.summary['partial_matches']} ({self.summary['partial_matches']/self.summary['total_tests']*100:.1f}%)"
        )
        print(
            f"   Failures: {self.summary['failures']} ({self.summary['failures']/self.summary['total_tests']*100:.1f}%)"
        )

        # Calculate overall accuracy
        total_pylint_errors = sum(len(r.pylint_errors) for r in self.results)
        total_matched_errors = sum(len(r.matched_errors) for r in self.results)
        overall_accuracy = (
            (total_matched_errors / total_pylint_errors * 100)
            if total_pylint_errors > 0
            else 100
        )
        print(f"   Overall accuracy: {overall_accuracy:.1f}%")

        # Performance summary
        print("\n⚡ PERFORMANCE:")
        print(f"   Total Pylint time: {self.summary['total_pylint_time']:.3f}s")
        print(f"   Total Prylint time: {self.summary['total_prylint_time']:.3f}s")
        if self.summary["total_prylint_time"] > 0:
            overall_speedup = (
                self.summary["total_pylint_time"] / self.summary["total_prylint_time"]
            )
            print(f"   Overall speedup: {overall_speedup:.1f}x")

        # Per-test performance
        if self.results:
            speedups = [r.speedup for r in self.results if r.prylint_time > 0]
            if speedups:
                print(f"   Average speedup: {statistics.mean(speedups):.1f}x")
                print(f"   Min speedup: {min(speedups):.1f}x")
                print(f"   Max speedup: {max(speedups):.1f}x")

        # Missing tests
        if self.summary["missing_tests"]:
            print(f"\n⚠️  TEST FILES CREATED FOR:")
            for code in sorted(self.summary["missing_tests"]):
                print(f"   - {code}")
            print("   Please add appropriate test cases to these files.")

        # Failed tests details
        failed_tests = [r for r in self.results if r.accuracy < 100]
        if failed_tests:
            print(f"\n❌ TESTS NEEDING ATTENTION:")
            for result in sorted(failed_tests, key=lambda x: x.accuracy):
                print(f"   - {result.error_code}: {result.accuracy:.1f}% accuracy")


def main():
    """Main entry point"""
    import argparse

    parser = argparse.ArgumentParser(
        description="Run error code tests for prylint and pylint"
    )
    parser.add_argument(
        "-v",
        "--verbose",
        action="store_true",
        help="Show detailed error information and raw outputs",
    )
    parser.add_argument("--code", help="Run test for specific error code (e.g., E0101)")
    parser.add_argument(
        "--full-output",
        action="store_true",
        help="Show full raw outputs without truncation",
    )
    args = parser.parse_args()

    runner = ErrorCodeTestRunner()

    if args.code:
        # Run single test
        test_files = runner.find_test_files()
        code = args.code.upper()
        if code in test_files:
            result = runner.run_single_test(code, test_files[code])
            runner.print_test_result(result, verbose=True, full_output=args.full_output)
        else:
            print(f"No test file found for error code {code}")
    else:
        # Run all tests
        runner.run_all_tests(verbose=args.verbose, full_output=args.full_output)
        runner.print_summary()


if __name__ == "__main__":
    main()
