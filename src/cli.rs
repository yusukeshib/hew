//! Command-line interface (clap).

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "hew", version, about = "review-first terminal patch viewer")]
pub struct Cli {
    /// Patch file to review. Omit or use `-` to read from stdin.
    #[arg(value_name = "FILE")]
    pub file: Option<PathBuf>,

    /// Load existing review comments from a sidecar JSON file (immutable input).
    #[arg(long, value_name = "FILE")]
    pub comments: Option<PathBuf>,
}
