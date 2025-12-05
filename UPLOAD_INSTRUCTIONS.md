# PyPI Upload Instructions for Pyrint

## Prerequisites

1. **Create a PyPI account** at https://pypi.org/account/register/
2. **Get an API token** from https://pypi.org/manage/account/token/
3. **Install required tools**:
   ```bash
   pip install --upgrade build twine
   ```

## Building the Package

Since Pyrint includes a Rust binary, we need to build platform-specific wheels:

### Step 1: Build the Rust binary with --release
```bash
cd /Users/adamraudonis/Desktop/Projects/Pyrint/pyrint
cargo build --release
```

### Step 2: Create the distribution
```bash
# Clean previous builds
rm -rf dist build *.egg-info

# Use the simple setup that includes the binary
python3 setup_simple.py bdist_wheel
```

### Step 3: Create source distribution (optional)
```bash
python3 setup_simple.py sdist
```

## Platform-Specific Considerations

**IMPORTANT**: The wheel you build will only work on your platform (macOS in this case). 
For a proper PyPI release, you would need to:

1. Build wheels for multiple platforms (Linux, Windows, macOS)
2. Use `cibuildwheel` to automate multi-platform builds
3. Or use GitHub Actions to build on different platforms

## Testing the Package Locally

Before uploading to PyPI, test the package locally:

```bash
# Create a virtual environment
python3 -m venv test_env
source test_env/bin/activate

# Install the wheel
pip install dist/pyrint-*.whl

# Test it
pyrint --version
pyrint some_script.py

# Deactivate when done
deactivate
```

## Uploading to TestPyPI (Recommended First)

TestPyPI is a separate instance for testing:

1. **Create account** at https://test.pypi.org/account/register/
2. **Upload**:
   ```bash
   python3 -m twine upload --repository testpypi dist/*
   ```
3. **Test installation**:
   ```bash
   pip install --index-url https://test.pypi.org/simple/ pyrint
   ```

## Uploading to PyPI

Once you're confident the package works:

```bash
# Upload to PyPI
python3 -m twine upload dist/*

# You'll be prompted for:
# Username: __token__
# Password: <your-api-token>
```

Or create a `~/.pypirc` file:
```ini
[pypi]
username = __token__
password = <your-api-token>
```

## Alternative: Pure Python Package

If you want broader compatibility without platform-specific builds:

1. **Option 1**: Have users install Rust and build on installation
   - Modify setup.py to build during install
   - Requires users to have Rust toolchain

2. **Option 2**: Download pre-built binaries
   - Host binaries on GitHub releases
   - Download appropriate binary during installation

3. **Option 3**: Use PyO3 for proper Python extension
   - Requires rewriting parts of the Rust code
   - Better integration with Python packaging

## Package Name Availability

Before uploading, check if 'pyrint' is available on PyPI:
- Visit: https://pypi.org/project/pyrint/
- If taken, you'll need to choose a different name in setup.py

## Versioning

Remember to update the version in:
- setup.py / setup_simple.py
- pyproject.toml
- pyrint_package/__init__.py
- Cargo.toml

## Continuous Deployment

For future releases, consider setting up:
- GitHub Actions for automated builds
- Automatic PyPI uploads on tags
- Multi-platform wheel building with cibuildwheel

## Current Status

✅ Package structure created
✅ Python wrapper module created
✅ Setup files configured
✅ README prepared for PyPI
✅ Rust binary built with --release
⚠️  Platform-specific wheel only (macOS)
⚠️  Manual upload required
⚠️  Multi-platform support needs additional setup

## Next Steps

1. Create PyPI account and get API token
2. Test the package locally
3. Upload to TestPyPI first
4. Fix any issues
5. Upload to production PyPI
6. Set up CI/CD for future releases