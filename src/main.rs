mod cli;
mod comments;
mod diff;
mod loader;
mod ui;

use anyhow::Result;
use clap::{CommandFactory, Parser};
use cli::Cli;
use std::io::IsTerminal;

fn main() -> Result<()> {
    let args = Cli::parse();

    // Bare `hew` with no file arg and an interactive stdin has nothing to
    // review: reading stdin would block on EOF (a Ctrl-D footgun). Show help
    // and exit non-zero instead. An explicit `-` still means "read stdin".
    if args.file.is_none() && std::io::stdin().is_terminal() {
        Cli::command().print_long_help()?;
        std::process::exit(2);
    }

    let changeset = loader::load_patch(args.file.as_deref())?;

    // `--comments <file>` is an immutable input: hew loads it as the review's
    // starting point and never writes back to it. A missing file just starts
    // from an empty base.
    let base = match &args.comments {
        Some(path) => loader::load_comments_or_default(path)?,
        None => comments::CommentStore::default(),
    };

    let final_comments = ui::run(changeset, base.clone())?;

    // Output is a compacted action log: the minimal review actions that turn
    // the immutable base into the final state. Always to stdout; an empty
    // session (no edits) prints `[]`.
    let actions = comments::diff(&base, &final_comments);
    println!("{}", serde_json::to_string_pretty(&actions)?);
    Ok(())
}
