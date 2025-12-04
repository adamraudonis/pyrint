use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use crate::Args;


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub jobs: usize,
    pub output_format: OutputFormat,
    pub enabled_checkers: HashSet<String>,
    pub disabled_checkers: HashSet<String>,
    pub ignore_patterns: Vec<String>,
    pub errors_only: bool,
    pub verbose: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OutputFormat {
    Text,
    Json,
    Parseable,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            jobs: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
            output_format: OutputFormat::Text,
            enabled_checkers: HashSet::new(),
            disabled_checkers: HashSet::new(),
            ignore_patterns: vec![],
            errors_only: false,
            verbose: false,
        }
    }
}

impl Config {
    pub fn from_args(args: &Args) -> Result<Self> {
        let mut config = if let Some(rcfile) = &args.rcfile {
            Self::from_file(rcfile)?
        } else {
            Self::from_default_locations()?
        };

        if args.jobs > 0 {
            config.jobs = args.jobs;
        }

        if let Some(format) = &args.output_format {
            config.output_format = match format.as_str() {
                "json" => OutputFormat::Json,
                "parseable" => OutputFormat::Parseable,
                _ => OutputFormat::Text,
            };
        }

        if let Some(enable) = &args.enable {
            for code in enable.split(',') {
                config.enabled_checkers.insert(code.trim().to_string());
            }
        }

        if let Some(disable) = &args.disable {
            for code in disable.split(',') {
                config.disabled_checkers.insert(code.trim().to_string());
            }
        }

        config.errors_only = args.errors_only;
        config.verbose = args.verbose;

        Ok(config)
    }

    pub fn from_file(path: &PathBuf) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        if path.extension().map_or(false, |ext| ext == "toml") {
            Ok(toml::from_str(&content)?)
        } else {
            Ok(serde_json::from_str(&content)?)
        }
    }

    pub fn from_default_locations() -> Result<Self> {
        let possible_paths = vec![
            PathBuf::from(".pyrintrc"),
            PathBuf::from(".pyrint.toml"),
            PathBuf::from("pyrint.toml"),
            PathBuf::from(".config/pyrint.toml"),
        ];

        for path in possible_paths {
            if path.exists() {
                return Self::from_file(&path);
            }
        }

        Ok(Self::default())
    }
}

