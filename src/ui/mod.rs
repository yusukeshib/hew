pub mod app;
pub mod render_rows;

use anyhow::Result;
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;
use std::io::stdout;

pub use app::WatchPaths;

use crate::comments::model::CommentStore;
use crate::diff::model::Changeset;

/// Set up the terminal, run the app, and restore the terminal afterwards.
/// When `watch` is `Some`, the listed files are reloaded on change.
pub fn run(
    title: String,
    changeset: Changeset,
    comments: CommentStore,
    watch: Option<WatchPaths>,
) -> Result<()> {
    // With nothing to watch, an empty changeset has nothing to show.
    if changeset.is_empty() && watch.is_none() {
        println!("hew: no changes to review");
        return Ok(());
    }

    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    let mut app = app::App::with_comments(title, changeset, comments);
    if let Some(w) = watch {
        app = app.watching(w);
    }
    let result = app.run(&mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}
