use anyhow::Result;
use clap::Parser;
use colored::*;
use std::process;

use pyrint::{Args, config::Config, linter::Linter, reporter::Reporter};

fn main() -> Result<()> {
    let args = Args::parse();

    if args.paths.is_empty() {
        eprintln!("{}: No files or directories specified", "Error".red().bold());
        process::exit(1);
    }

    let config = Config::from_args(&args)?;
    let mut linter = Linter::new(config);

    let mut exit_code = 0;
    for path in &args.paths {
        match linter.check_path(path) {
            Ok(issues) => {
                if !issues.is_empty() {
                    exit_code = 1;
                    let reporter = Reporter::new(args.output_format.as_deref());
                    reporter.report(&issues)?;
                }
            }
            Err(e) => {
                eprintln!("{}: {}", "Error".red().bold(), e);
                exit_code = 2;
            }
        }
    }

    process::exit(exit_code);
}