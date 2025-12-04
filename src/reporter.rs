use anyhow::Result;
use colored::*;
use serde_json;
use std::collections::BTreeMap;
use std::path::Path;

use crate::config::OutputFormat;
use crate::errors::{Issue, Severity};

pub struct Reporter {
    format: OutputFormat,
}

impl Reporter {
    pub fn new(format: Option<&str>) -> Self {
        let format = match format {
            Some("json") => OutputFormat::Json,
            Some("parseable") => OutputFormat::Parseable,
            _ => OutputFormat::Text,
        };
        Self { format }
    }

    pub fn report(&self, issues: &[Issue]) -> Result<()> {
        if issues.is_empty() {
            return Ok(());
        }

        match self.format {
            OutputFormat::Text => self.report_text(issues),
            OutputFormat::Json => self.report_json(issues),
            OutputFormat::Parseable => self.report_parseable(issues),
        }
    }

    fn report_text(&self, issues: &[Issue]) -> Result<()> {
        let mut issues_by_file: BTreeMap<&Path, Vec<&Issue>> = BTreeMap::new();
        
        for issue in issues {
            issues_by_file
                .entry(&issue.file)
                .or_insert_with(Vec::new)
                .push(issue);
        }

        for (file, file_issues) in issues_by_file {
            println!("\n{}", format!("************* Module {}", file.display()).bold());
            
            let mut sorted_issues = file_issues;
            sorted_issues.sort_by_key(|i| (i.line, i.column));
            
            for issue in sorted_issues {
                let severity_str = match issue.severity {
                    Severity::Error => format!("{}", issue.code).red().bold(),
                    Severity::Warning => format!("{}", issue.code).yellow().bold(),
                    Severity::Convention => format!("{}", issue.code).blue().bold(),
                    Severity::Refactor => format!("{}", issue.code).magenta().bold(),
                    Severity::Information => format!("{}", issue.code).cyan().bold(),
                };

                println!(
                    "{}:{}:{}: {}: {} ({})",
                    file.display(),
                    issue.line,
                    issue.column,
                    severity_str,
                    issue.message,
                    issue.symbol.dimmed()
                );
            }
        }

        self.print_summary(issues);
        Ok(())
    }

    fn report_json(&self, issues: &[Issue]) -> Result<()> {
        let json_output = serde_json::to_string_pretty(issues)?;
        println!("{}", json_output);
        Ok(())
    }

    fn report_parseable(&self, issues: &[Issue]) -> Result<()> {
        for issue in issues {
            println!(
                "{}:{}:{}: [{}] {}",
                issue.file.display(),
                issue.line,
                issue.column,
                issue.code,
                issue.message
            );
        }
        Ok(())
    }

    fn print_summary(&self, issues: &[Issue]) {
        let mut error_count = 0;
        let mut warning_count = 0;
        let mut convention_count = 0;
        let mut refactor_count = 0;
        let mut info_count = 0;

        for issue in issues {
            match issue.severity {
                Severity::Error => error_count += 1,
                Severity::Warning => warning_count += 1,
                Severity::Convention => convention_count += 1,
                Severity::Refactor => refactor_count += 1,
                Severity::Information => info_count += 1,
            }
        }

        println!("\n{}", "Summary:".bold().underline());
        
        if error_count > 0 {
            println!("  {} error(s)", error_count.to_string().red().bold());
        }
        if warning_count > 0 {
            println!("  {} warning(s)", warning_count.to_string().yellow().bold());
        }
        if convention_count > 0 {
            println!("  {} convention(s)", convention_count.to_string().blue().bold());
        }
        if refactor_count > 0 {
            println!("  {} refactor(s)", refactor_count.to_string().magenta().bold());
        }
        if info_count > 0 {
            println!("  {} info(s)", info_count.to_string().cyan().bold());
        }
    }
}