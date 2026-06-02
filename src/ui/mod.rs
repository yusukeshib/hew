pub mod app;
pub mod render_rows;

use anyhow::Result;
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;
use std::io::stdout;

use crate::comments::model::CommentStore;
use crate::diff::model::Changeset;

/// Set up the terminal, run the app, and restore the terminal afterwards.
pub fn run(title: String, changeset: Changeset, comments: CommentStore) -> Result<()> {
    if changeset.is_empty() {
        println!("hew: no changes to review");
        return Ok(());
    }

    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    let mut app = app::App::with_comments(title, changeset, comments);
    let result = app.run(&mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}
