//! Command-line interface (clap).

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "hew", version, about = "review-first terminal diff viewer")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Review changes: working tree, or compare two files.
    Diff {
        /// Two files to compare (old new). Omit for the git working tree.
        #[arg(value_name = "FILE")]
        files: Vec<PathBuf>,
    },
    /// Review a commit (defaults to HEAD).
    Show {
        #[arg(default_value = "HEAD")]
        rev: String,
    },
    /// Review a unified patch from a file or stdin (`-`).
    Patch {
        #[arg(value_name = "FILE")]
        file: Option<PathBuf>,
    },
    /// Print the parsed changeset as JSON and exit (no TUI).
    Review {
        /// Same inputs as `diff`: two files, or empty for the working tree.
        #[arg(value_name = "FILE")]
        files: Vec<PathBuf>,
        /// Read a patch from this file or stdin (`-`) instead of git.
        #[arg(long, value_name = "FILE")]
        patch: Option<PathBuf>,
    },
}
