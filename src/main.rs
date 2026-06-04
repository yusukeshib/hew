mod cli;
mod comments;
mod diff;
mod loader;
mod session;
mod ui;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Command, CommentAction};

fn main() -> Result<()> {
    let args = Cli::parse();

    // Client subcommands talk to a running session over its socket.
    if let Some(command) = args.command {
        return run_command(command);
    }

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

    // Advertise this session so `hew comment …` can reach it. A failure here
    // (e.g. no writable registry dir) shouldn't stop the review, so it's
    // best-effort. The handle lives until the TUI exits, then deregisters.
    let files: Vec<String> = changeset
        .files
        .iter()
        .map(|f| f.display_path().to_string())
        .collect();
    let (_session, ipc) = match session::start(args.name.clone(), files) {
        Ok((s, rx)) => (Some(s), Some(rx)),
        Err(e) => {
            eprintln!("hew: session registration failed ({e}); continuing without IPC");
            (None, None)
        }
    };

    let final_comments = ui::run(changeset, base.clone(), watch, ipc)?;

    // Output is a compacted action log: the minimal review actions that turn
    // the immutable base into the final state. Always to stdout; an empty
    // session (no edits) prints `[]`.
    let actions = comments::diff(&base, &final_comments);
    println!("{}", serde_json::to_string_pretty(&actions)?);
    Ok(())
}

/// Handle a client subcommand (`hew comment …`).
fn run_command(command: Command) -> Result<()> {
    match command {
        Command::Comment {
            action: CommentAction::List { session: selector },
        } => {
            let target = session::resolve_target(selector.as_deref())?;
            let resp = session::query(&target.sock, "{\"cmd\":\"list\"}")?;
            println!("{resp}");
            Ok(())
        }
    }
}
