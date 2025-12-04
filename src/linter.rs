use anyhow::{Context, Result};
use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::ast_visitor::AstContext;
use crate::config::Config;
use crate::errors::Issue;

pub struct Linter {
    config: Config,
}

impl Linter {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub fn check_path(&mut self, path: &Path) -> Result<Vec<Issue>> {
        if path.is_file() {
            self.check_file(path)
        } else if path.is_dir() {
            self.check_directory(path)
        } else {
            Err(anyhow::anyhow!("Path does not exist: {:?}", path))
        }
    }

    pub fn check_directory(&mut self, dir: &Path) -> Result<Vec<Issue>> {
        let python_files: Vec<PathBuf> = WalkDir::new(dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().is_file()
                    && e.path()
                        .extension()
                        .map_or(false, |ext| ext == "py" || ext == "pyi")
            })
            .filter(|e| !self.should_ignore(e.path()))
            .map(|e| e.path().to_path_buf())
            .collect();

        let issues: Vec<Issue> = if self.config.jobs > 1 {
            python_files
                .par_iter()
                .map(|file| self.check_file(file))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect()
        } else {
            python_files
                .iter()
                .map(|file| self.check_file(file))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect()
        };

        Ok(issues)
    }

    pub fn check_file(&self, file: &Path) -> Result<Vec<Issue>> {
        let source = fs::read_to_string(file)
            .with_context(|| format!("Failed to read file: {:?}", file))?;

        if source.is_empty() {
            return Ok(Vec::new());
        }

        let mut context = AstContext::new(file, source);
        
        match context.parse_and_check() {
            Ok(_) => {}
            Err(_) => {}
        }

        let issues = context.issues.clone();
        
        let filtered_issues = if !self.config.enabled_checkers.is_empty() {
            issues
                .into_iter()
                .filter(|issue| self.config.enabled_checkers.contains(&issue.code))
                .collect()
        } else if !self.config.disabled_checkers.is_empty() {
            issues
                .into_iter()
                .filter(|issue| !self.config.disabled_checkers.contains(&issue.code))
                .collect()
        } else {
            issues
        };

        Ok(filtered_issues)
    }

    fn should_ignore(&self, path: &Path) -> bool {
        for pattern in &self.config.ignore_patterns {
            if path.to_string_lossy().contains(pattern) {
                return true;
            }
        }
        
        path.components().any(|c| {
            c.as_os_str() == "__pycache__"
                || c.as_os_str() == ".git"
                || c.as_os_str() == ".venv"
                || c.as_os_str() == "venv"
                || c.as_os_str() == ".tox"
                || c.as_os_str() == ".mypy_cache"
                || c.as_os_str() == ".pytest_cache"
        })
    }
}