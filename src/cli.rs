//! Command-line interface (clap).

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "hew", version, about = "review-first terminal patch viewer")]
pub struct Cli {
    /// Patch file to review. Omit or use `-` to read from stdin.
    #[arg(value_name = "FILE")]
    pub file: Option<PathBuf>,

    /// Load review comments from a sidecar JSON file before opening.
    #[arg(long, value_name = "FILE")]
    pub comments: Option<PathBuf>,

    /// Print the parsed changeset as JSON and exit (no TUI).
    #[arg(long)]
    pub json: bool,

    /// Reload the patch file and/or comments sidecar when they change on disk.
    /// (Has no effect when the patch is read from stdin.)
    #[arg(long)]
    pub watch: bool,
}
