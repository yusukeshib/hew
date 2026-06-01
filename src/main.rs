mod cli;
mod comments;
mod diff;
mod loader;
mod ui;
mod vcs;

use anyhow::{bail, Result};
use clap::Parser;
use cli::{Cli, Command};
use loader::Source;
use std::env;

fn diff_source(files: &[std::path::PathBuf]) -> Result<Source> {
    Ok(match files.len() {
        0 => Source::WorkingTree { repo: env::current_dir()? },
        2 => Source::TwoFiles { old: files[0].clone(), new: files[1].clone() },
        n => bail!("expected 0 or 2 files, got {n}"),
    })
}

fn main() -> Result<()> {
    let args = Cli::parse();

    if let Command::Review { files, patch } = &args.command {
        let source = match patch {
            Some(p) => Source::Patch { path: Some(p.clone()) },
            None => diff_source(files)?,
        };
        let changeset = loader::load(&source)?;
        println!("{}", serde_json::to_string_pretty(&changeset)?);
        return Ok(());
    }

    let source = match args.command {
        Command::Diff { files } => diff_source(&files)?,
        Command::Show { rev } => Source::Show { repo: env::current_dir()?, rev },
        Command::Patch { file } => Source::Patch { path: file },
        Command::Review { .. } => unreachable!(),
    };

    let title = source.label();
    let changeset = loader::load(&source)?;
    ui::run(title, changeset)
}
