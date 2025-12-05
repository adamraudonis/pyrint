"""Simple setup configuration for pyrint package without PyO3."""

from setuptools import setup, find_packages
from setuptools.command.build_py import build_py
from setuptools.command.develop import develop
from setuptools.command.install import install
import os
import shutil
import subprocess
import platform


def build_rust_binary():
    """Build the Rust binary with --release flag."""
    print("Building Rust binary with --release...")
    result = subprocess.run(
        ["cargo", "build", "--release"],
        cwd=os.path.dirname(os.path.abspath(__file__)),
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        print(f"Error building Rust binary: {result.stderr}")
        raise RuntimeError("Failed to build Rust binary")
    print("Rust binary built successfully!")


def copy_binary_to_package():
    """Copy the built binary to the package directory."""
    src_binary = os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "target", "release", "pyrint"
    )

    # Add .exe extension on Windows
    if platform.system() == "Windows":
        src_binary += ".exe"

    if not os.path.exists(src_binary):
        raise FileNotFoundError(
            f"Binary not found at {src_binary}. Run 'cargo build --release' first."
        )

    dst_dir = os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "pyrint_package", "bin"
    )
    os.makedirs(dst_dir, exist_ok=True)

    dst_binary = os.path.join(dst_dir, "pyrint")
    if platform.system() == "Windows":
        dst_binary += ".exe"

    shutil.copy2(src_binary, dst_binary)
    # Make it executable on Unix-like systems
    if platform.system() != "Windows":
        os.chmod(dst_binary, 0o755)

    print(f"Binary copied to {dst_binary}")


class CustomBuildPy(build_py):
    def run(self):
        build_rust_binary()
        copy_binary_to_package()
        build_py.run(self)


class CustomDevelop(develop):
    def run(self):
        build_rust_binary()
        copy_binary_to_package()
        develop.run(self)


class CustomInstall(install):
    def run(self):
        build_rust_binary()
        copy_binary_to_package()
        install.run(self)


# Read the long description from README
def read_long_description():
    readme_path = os.path.join(os.path.dirname(__file__), "README.md")
    if os.path.exists(readme_path):
        with open(readme_path, "r", encoding="utf-8") as f:
            return f.read()
    return ""


setup(
    name="pyrint",
    version="0.1.0",
    author="Adam Raudonis",
    author_email="adam.raudonis@gmail.com",  # Replace with your email
    description="A fast Python linter written in Rust",
    long_description=read_long_description(),
    long_description_content_type="text/markdown",
    url="https://github.com/adamraudonis/pyrint",
    project_urls={
        "Bug Tracker": "https://github.com/adamraudonis/pyrint/issues",
        "Source Code": "https://github.com/adamraudonis/pyrint",
    },
    classifiers=[
        "Development Status :: 3 - Alpha",
        "Intended Audience :: Developers",
        "License :: OSI Approved :: MIT License",
        "Programming Language :: Python :: 3",
        "Programming Language :: Python :: 3.8",
        "Programming Language :: Python :: 3.9",
        "Programming Language :: Python :: 3.10",
        "Programming Language :: Python :: 3.11",
        "Programming Language :: Python :: 3.12",
        "Programming Language :: Rust",
        "Topic :: Software Development :: Quality Assurance",
        "Topic :: Software Development :: Testing",
        "Operating System :: OS Independent",
    ],
    python_requires=">=3.8",
    packages=["pyrint"],
    package_dir={"pyrint": "pyrint_package"},
    package_data={
        "pyrint": ["bin/*"],
    },
    include_package_data=True,
    cmdclass={
        "build_py": CustomBuildPy,
        "develop": CustomDevelop,
        "install": CustomInstall,
    },
    entry_points={
        "console_scripts": [
            "pyrint=pyrint.cli:main",
        ],
    },
    zip_safe=False,
)
