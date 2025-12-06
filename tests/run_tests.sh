#!/bin/bash
# Simple script to run all error code tests

echo "========================================="
echo "Running Prylint vs Pylint Error Code Tests"
echo "========================================="

# Run the comprehensive error code test runner
python3 "$(dirname "$0")/run_error_code_tests.py" "$@"