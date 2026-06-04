mod cli;
mod comments;
mod diff;
mod loader;
mod ui;

use anyhow::Result;
use clap::Parser;
use cli::Cli;

fn main() -> Result<()> {
    let args = Cli::parse();

    let changeset = loader::load_patch(args.file.as_deref())?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&changeset)?);
        return Ok(());
    }

    // `--comments <file>` is an immutable input: hew loads it as the review's
    // starting point and never writes back to it. A missing file just starts
    // from an empty base.
    let base = match &args.comments {
        Some(path) => loader::load_comments_or_default(path)?,
        None => comments::CommentStore::default(),
    };

    // --watch reloads the patch when it changes on disk. The --comments base is
    // immutable, so it is never watched; a stdin patch can't be re-read either,
    // so there's nothing to watch in that case.
    let watch = if args.watch {
        args.file
            .as_ref()
            .filter(|p| p.as_os_str() != "-")
            .cloned()
            .map(|patch| ui::WatchPaths { patch: Some(patch) })
    } else {
        None
    };

    let final_comments = ui::run(changeset, base.clone(), watch)?;

    // Output is a compacted action log: the minimal review actions that turn
    // the immutable base into the final state. Always to stdout; an empty
    // session (no edits) prints `[]`.
    let actions = comments::diff(&base, &final_comments);
    println!("{}", serde_json::to_string_pretty(&actions)?);
    Ok(())
}
