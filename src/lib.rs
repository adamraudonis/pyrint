pub mod ast_visitor;
pub mod checkers;
pub mod config;
pub mod errors;
pub mod linter;
pub mod reporter;

// Re-export Args for library usage
use clap::Parser;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct Args {
    #[clap(help = "Python files or directories to lint")]
    pub paths: Vec<std::path::PathBuf>,

    #[clap(short = 'e', long = "errors-only", short_alias = 'E', help = "Display only error messages")]
    pub errors_only: bool,

    #[clap(short = 'f', long, help = "Output format (text, json, parseable)")]
    pub output_format: Option<String>,

    #[clap(short = 'j', long, help = "Number of parallel jobs", default_value = "0")]
    pub jobs: usize,

    #[clap(long, help = "Configuration file")]
    pub rcfile: Option<std::path::PathBuf>,

    #[clap(long, help = "Enable specific checkers (comma-separated)")]
    pub enable: Option<String>,

    #[clap(long, help = "Disable specific checkers (comma-separated)")]
    pub disable: Option<String>,

    #[clap(short, long, help = "Increase verbosity")]
    pub verbose: bool,
}