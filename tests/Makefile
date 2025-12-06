.PHONY: all test verbose full clean help test-code test-code-verbose

# Default target
all: test

# Run all error code tests
test:
	@echo "Running error code tests..."
	@python3 run_error_code_tests.py

# Run tests with verbose output (shows raw pylint/prylint outputs)
verbose:
	@echo "Running error code tests (verbose)..."
	@python3 run_error_code_tests.py -v

# Run tests with full output (verbose + no truncation)
full:
	@echo "Running error code tests (full output)..."
	@python3 run_error_code_tests.py -v --full-output

# Run test for a specific error code
test-code:
	@if [ -z "$(CODE)" ]; then \
		echo "Usage: make test-code CODE=E0101"; \
		exit 1; \
	fi
	@echo "Testing error code $(CODE)..."
	@python3 run_error_code_tests.py --code $(CODE)

# Run test for a specific error code with verbose output
test-code-verbose:
	@if [ -z "$(CODE)" ]; then \
		echo "Usage: make test-code-verbose CODE=E0101"; \
		exit 1; \
	fi
	@echo "Testing error code $(CODE) (verbose)..."
	@python3 run_error_code_tests.py --code $(CODE) -v

# Clean up any generated files
clean:
	@echo "Cleaning up..."
	@find . -type f -name "*.pyc" -delete
	@find . -type d -name "__pycache__" -delete

# Show help
help:
	@echo "Available targets:"
	@echo "  make test               - Run all error code tests"
	@echo "  make verbose            - Run tests with raw outputs"
	@echo "  make full               - Run tests with full raw outputs (no truncation)"
	@echo "  make test-code CODE=E0101      - Test specific error code"
	@echo "  make test-code-verbose CODE=E0101 - Test specific code with verbose output"
	@echo "  make clean              - Clean up generated files"
	@echo "  make help               - Show this help message"