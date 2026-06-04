//! Command-line interface (clap).

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "hew", version, about = "review-first terminal patch viewer")]
// When a subcommand is used, the viewer args are not (and vice versa).
#[command(args_conflicts_with_subcommands = true)]
pub struct Cli {
    /// Talk to a running hew session (`hew comment …`).
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Patch file to review. Omit or use `-` to read from stdin.
    #[arg(value_name = "FILE")]
    pub file: Option<PathBuf>,

    /// Load review comments from a sidecar JSON file before opening.
    #[arg(long, value_name = "FILE")]
    pub comments: Option<PathBuf>,

    /// Name this session in the registry (defaults to a short id).
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    /// Print the parsed changeset as JSON and exit (no TUI).
    #[arg(long)]
    pub json: bool,

    /// Reload the patch file and/or comments sidecar when they change on disk.
    /// (Has no effect when the patch is read from stdin.)
    #[arg(long)]
    pub watch: bool,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Inspect or edit comments in a running hew session.
    Comment {
        #[command(subcommand)]
        action: CommentAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum CommentAction {
    /// Print the session's current review store as JSON.
    List {
        /// Target session id or name (required when several are running).
        #[arg(long, value_name = "ID")]
        session: Option<String>,
    },
}
