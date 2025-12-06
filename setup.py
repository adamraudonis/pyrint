"""Setup configuration for prylint package."""

from setuptools import setup, find_packages
from setuptools_rust import Binding, RustExtension
import os


# Read the long description from README
def read_long_description():
    readme_path = os.path.join(os.path.dirname(__file__), "README.md")
    if os.path.exists(readme_path):
        with open(readme_path, "r", encoding="utf-8") as f:
            return f.read()
    return ""


setup(
    name="prylint",
    version="0.1.0",
    author="Adam Raudonis",
    author_email="adam.raudonis@gmail.com",  # Replace with your email
    description="A fast Python linter written in Rust",
    long_description=read_long_description(),
    long_description_content_type="text/markdown",
    url="https://github.com/adamraudonis/prylint",  # Replace with your repo URL
    project_urls={
        "Bug Tracker": "https://github.com/adamraudonis/prylint/issues",
        "Source Code": "https://github.com/adamraudonis/prylint",
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
    ],
    python_requires=">=3.8",
    packages=find_packages(where=".", exclude=["tests", "tests.*"]),
    rust_extensions=[
        RustExtension(
            "prylint._prylint_rust",
            path="Cargo.toml",
            binding=Binding.PyO3,
        )
    ],
    install_requires=[
        # Core dependencies only
    ],
    extras_require={
        "dev": [
            "pytest>=7.0",
            "pylint>=2.0",
        ],
    },
    entry_points={
        "console_scripts": [
            "prylint=prylint.cli:main",
        ],
    },
    include_package_data=True,
    zip_safe=False,
)
