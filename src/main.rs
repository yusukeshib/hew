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

    let title = match &args.file {
        Some(p) if p.as_os_str() != "-" => format!("patch {}", p.display()),
        _ => "patch -".into(),
    };

    let changeset = loader::load_patch(args.file.as_deref())?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&changeset)?);
        return Ok(());
    }

    let comments = match &args.comments {
        Some(path) => loader::load_comments(path)?,
        None => comments::CommentStore::default(),
    };

    ui::run(title, changeset, comments)
}
