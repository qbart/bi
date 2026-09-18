//! bi's GUI frontend.
//!
//! Argument parsing, editor setup and the gpui application, and nothing else.
//! The editor is the `bi` library; the window is [`view::View`]. See
//! `docs/specs/gui.md`.

mod cells;
mod keys;
mod view;

use anyhow::{Result, bail};
use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};

use bi::config::Xdg;
use bi::editor::Editor;

/// gpui and the stack under it report through `log`, and a window has no
/// stderr anyone watches — except when it draws nothing, which is when
/// `BI_GUI_LOG=1` puts everything they say on it.
struct Stderr;

impl log::Log for Stderr {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, r: &log::Record) {
        eprintln!("[{}] {}: {}", r.level(), r.target(), r.args());
    }
    fn flush(&self) {}
}

fn main() -> Result<()> {
    if std::env::var_os("BI_GUI_LOG").is_some() {
        let _ = log::set_logger(&Stderr).map(|()| log::set_max_level(log::LevelFilter::Debug));
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = match args.as_slice() {
        [] => None,
        [one] => Some(one.clone()),
        _ => bail!("usage: bi-gui [path]"),
    };

    let mut editor = match path {
        Some(path) => Editor::open(path)?,
        None => Editor::empty(),
    };

    // The same file the terminal reads, through the same library code: the
    // theme and the keymap are in it, and a frontend without them draws in no
    // colours and answers no keys. Nothing else the terminal attaches —
    // clipboard, language servers, debug adapters, shells, formatters, git —
    // is attached here yet; an editor whose frontend never calls those
    // setters attaches nothing, which is the design.
    let problems = editor.load_config(Xdg::from_env());
    if !problems.is_empty() {
        let n = problems.len();
        editor.session.status =
            format!("{n} config problem{}: {}", if n == 1 { "" } else { "s" }, problems[0].message);
    }

    Application::new().run(move |cx: &mut App| {
        // The window's close button is `:q` without the modified check — see
        // the spec. One window, so its closing is the application's.
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(960.), px(640.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions { title: Some("bi".into()), ..Default::default() }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| view::View::new(editor, window, cx)),
        )
        .expect("opening the window");
        cx.activate(true);
    });
    Ok(())
}
