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

    let comments = match &args.comments {
        Some(path) => loader::load_comments(path)?,
        None => comments::CommentStore::default(),
    };

    // --watch reloads file inputs; a stdin patch can't be re-read, so only
    // watch when there is at least one real file to poll.
    let watch = if args.watch {
        let patch = args.file.as_ref().filter(|p| p.as_os_str() != "-").cloned();
        let comments_path = args.comments.clone();
        if patch.is_some() || comments_path.is_some() {
            Some(ui::WatchPaths {
                patch,
                comments: comments_path,
            })
        } else {
            None
        }
    } else {
        None
    };

    ui::run(changeset, comments, watch)
}
