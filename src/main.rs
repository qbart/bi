//! bee — a batteries-included modal editor.
//!
//! Step 1: the core loop. Open a file, move, edit, save, quit.
//! Tree-sitter, git, and LSP come after this foundation is proven, and the
//! seams they need ([`buffer::Edit`], the [`editor::Action`] table, viewport-
//! bounded rendering) exist already.

mod buffer;
mod editor;
mod input;
mod ui;

use std::io::{self, Stdout};
use std::panic;

use anyhow::{Context, Result};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::crossterm::{execute, terminal};

use editor::Editor;
use input::Input;

type Term = Terminal<CrosstermBackend<Stdout>>;

fn main() -> Result<()> {
    let mut editor = match std::env::args().nth(1) {
        Some(path) => Editor::open(path)?,
        None => Editor::empty(),
    };

    let mut term = setup().context("entering raw mode")?;
    let result = run(&mut term, &mut editor);
    restore()?;
    result
}

fn setup() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    // Without this, a panic anywhere leaves the user in a wrecked terminal with
    // no echo and no prompt.
    let hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = restore();
        hook(info);
    }));

    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore() -> Result<()> {
    if terminal::is_raw_mode_enabled()? {
        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen)?;
    }
    Ok(())
}

fn run(term: &mut Term, ed: &mut Editor) -> Result<()> {
    let mut input = Input::default();

    loop {
        term.draw(|frame| ui::render(frame, ed, &input.pending_display()))?;

        match event::read()? {
            // Windows terminals emit Release too; without this filter every key
            // fires twice.
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if let Some(cmd) = input.on_key(key, &ed.mode) {
                    ed.status.clear();
                    ed.apply(cmd);
                }
                // Drop what tree-sitter and LSP will one day consume.
                ed.buffer.pending_edits.clear();
            }
            Event::Resize(_, _) => {}
            _ => {}
        }

        if ed.quit {
            return Ok(());
        }
    }
}
