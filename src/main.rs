//! bi's terminal frontend.
//!
//! Terminal setup, the event loop, and nothing else. The editor itself is the
//! `bi` library — see `src/lib.rs`. A second frontend would replace this file
//! and `src/tui/`, and touch nothing below them.

mod tui;

use std::io::{self, Stdout};
use std::panic;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange,
    Event, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::crossterm::{execute, terminal};

use bi::config::Xdg;
use bi::config::xdg::config_dir;
use bi::editor::Editor;
use bi::input::Input;

type Term = Terminal<CrosstermBackend<Stdout>>;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let path = match parse_args(&args)? {
        Invocation::ConfigInit => {
            let dir = config_dir().context("no HOME and no XDG_CONFIG_HOME — nowhere to write")?;
            return config_init(&dir);
        }
        Invocation::ConfigEdit => {
            let dir = config_dir().context("no HOME and no XDG_CONFIG_HOME — nowhere to look")?;
            Some(config_edit_path(&dir)?.to_string_lossy().into_owned())
        }
        Invocation::DebugInit => {
            let dir = std::env::current_dir().context("no working directory to write into")?;
            match debug_init(&dir)? {
                DebugInit::Wrote(path) => println!("wrote {}", path.display()),
                // The sample still reaches the user — on stdout, where it
                // can be redirected or read — and the warning goes where a
                // warning belongs, so `bi debug init > x` stays clean.
                DebugInit::Existed(path) => {
                    print!("{}", debug_menu());
                    eprintln!(
                        "{} already exists — printed the sample instead of creating it",
                        path.display()
                    );
                }
            }
            return Ok(());
        }
        Invocation::Open(path) => path,
    };

    let mut editor = match path {
        Some(path) => Editor::open(path)?,
        None => Editor::empty(),
    };

    // Before the config, so the theme resolves once rather than twice — but
    // `set_remote` re-resolves either way, so the order is a nicety and not a
    // requirement. Reading the environment is the frontend's job: it is
    // process-wide state, and an embedder that is not a terminal has no
    // SSH_CONNECTION to consult.
    editor.set_remote(std::env::var_os("SSH_CONNECTION").is_some());
    let problems = editor.load_config(Xdg::from_env());
    // `"+y` and `"+p`. The library holds the trait; the terminal is what knows
    // how to reach a clipboard from inside one.
    editor.set_clipboard(tui::clipboard::Osc52);
    // Language servers arrive the same way the clipboard does: the library
    // holds the trait, and spawning processes is a fact about the host. An
    // editor whose frontend never calls this attaches nothing.
    editor.set_lsp_spawner(bi::lsp::transport::ProcessSpawn);
    // Debug adapters arrive the same way — a DAP adapter is a child process
    // like a language server, and spawning one is the host's business too.
    // See docs/specs/debug.md.
    editor.set_dap_spawner(bi::dap::transport::ProcessSpawn);
    // `:!` jobs too — a shell command is another child process, and the
    // library never spawns one itself. See docs/specs/shell.md.
    editor.set_shell_spawner(bi::shell::ProcessSpawn);
    // External formatters too — `:fmt` through a tool is a child process,
    // and processes are the host's business. See docs/specs/fmt.md.
    editor.set_fmt_runner(bi::fmt::ProcessRun::default());
    editor.set_lsp_installer(bi::lsp::transport::ProcessInstall);
    // Git arrives the same way: the library computes the diff, and running
    // `git` is a fact about the host. See docs/specs/git-signs.md.
    editor.set_git_baseline(bi::git::baseline);

    let mut term = setup().context("entering raw mode")?;
    // After raw mode is on, before the event-reader thread takes stdin: the
    // handshake reads the terminal's answers itself. See src/tui/graphics.rs.
    let graphics = tui::graphics::detect();
    let mut gfx = tui::graphics::Graphics::new(graphics);

    if !problems.is_empty() {
        let n = problems.len();
        editor.session.status =
            format!("{n} config problem{}: {}", if n == 1 { "" } else { "s" }, problems[0].message);
    }

    let result = run(&mut term, &mut editor, &mut gfx);
    // Before `restore`, not after: the servers get their shutdown while the
    // screen is still bi's, and a hung one is killed rather than waited on.
    editor.shutdown_lsp();
    // And the debug adapters, for the stronger version of the same reason:
    // an adapter outliving bi keeps a *stopped debuggee* alive behind it.
    editor.shutdown_dap();
    // And a running `:!` job — quitting on top of it must not orphan it.
    editor.shutdown_shell();
    // Also before `restore`, for the same reason: leaving the alternate
    // screen does not take a kitty placement with it, and an editor that
    // quits leaving a photograph floating over the shell has not quit.
    let _ = gfx.clear();
    restore()?;
    result
}

/// What the command line asked for.
enum Invocation {
    Open(Option<String>),
    ConfigInit,
    ConfigEdit,
    /// `bi debug init` — a project's `.bi.toml` seeded with launch configs.
    DebugInit,
}

/// `config` and `debug` are subcommands only in the two-word form, so a file
/// actually named `config` still opens.
fn parse_args(args: &[String]) -> Result<Invocation> {
    match args {
        [] => Ok(Invocation::Open(None)),
        [one] => Ok(Invocation::Open(Some(one.clone()))),
        [first, sub] if first == "config" => match sub.as_str() {
            "init" => Ok(Invocation::ConfigInit),
            "edit" => Ok(Invocation::ConfigEdit),
            other => bail!("no such command: bi config {other} — try `init` or `edit`"),
        },
        [first, sub] if first == "debug" => match sub.as_str() {
            "init" => Ok(Invocation::DebugInit),
            other => bail!("no such command: bi debug {other} — try `init`"),
        },
        _ => bail!("usage: bi [path] | bi config init | bi config edit | bi debug init"),
    }
}

/// The header on a freshly written config, explaining the one thing a user
/// has to know about the file.
const INIT_HEADER: &str = "\
# bi config
#
# This file is a PATCH over bi's defaults, not a replacement. Anything left
# commented out keeps doing what bi does by default, including settings added
# in later versions. Uncomment a line only to change it.
#
# `:reload` re-reads this file without restarting.

";

/// bi's defaults, commented out.
///
/// Written live they would silently turn every user's file into a full
/// replacement, and that user would stop receiving defaults bi adds later —
/// invisibly and permanently. Commented out it is a self-documenting menu that
/// is semantically empty.
///
/// Section headers (`[options]`, `[keys.normal]` and friends) are
/// written *live*, uncommented, even though every key beneath one is
/// commented out. An empty table parses to nothing — `read_options` walks a
/// table with no items and produces no diagnostics — so the file stays
/// semantically empty either way. The alternative, commenting the header too,
/// turns "uncomment the one line you want" into a lie: the key would then sit
/// outside any table and the parser would correctly reject it as not being in
/// a section.
fn commented(defaults: &str) -> String {
    let mut out = String::from(INIT_HEADER);
    for line in defaults.lines() {
        let trimmed = line.trim_start();
        // Assumes every setting is scalar-valued, one per line. A multi-line
        // array value's continuation line can itself start with `[` — an
        // array of arrays, say — and would be mistaken for a table header
        // here and left live instead of commented. True of every option
        // today; worth another look the day one isn't.
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('[') {
            out.push_str(line);
            out.push('\n');
        } else {
            out.push_str("# ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Every default bi has, as the file that would set them.
///
/// `default.toml` holds the options and the leader; the keymap is still `match`
/// arms in `input.rs`, so its half is *generated* from the names table rather
/// than written out here. That matters more than the tidiness: a hand-kept copy
/// of 90 bindings beside the real ones is a second source of truth, and the day
/// they disagree the file is worse than useless. Generated, the listing cannot
/// say anything the parser would not accept.
fn defaults() -> String {
    format!("{}\n{}", bi::config::DEFAULT_TOML.trim_end(), bi::config::listing())
}

/// Creates the config directory and writes `config.toml` if it is absent.
///
/// Never automatic: a config file appears because you asked for one.
fn config_init(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let path = dir.join("config.toml");
    if path.exists() {
        println!("{} already exists — leaving it alone", path.display());
        return Ok(());
    }

    std::fs::write(&path, commented(&defaults()))
        .with_context(|| format!("writing {}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

/// What `bi config edit` opens: the config *directory*, so `themes/` is in
/// the tree beside `config.toml`.
///
/// It does not create anything. `bi config init` is the manual step, and
/// `edit` surprising you with a new file would undo that.
fn config_edit_path(dir: &Path) -> Result<PathBuf> {
    if !dir.exists() {
        bail!("no config yet — run `bi config init`");
    }
    Ok(dir.to_path_buf())
}

/// The header on a freshly written `.bi.toml`: what the file is, and the
/// one rule that differs from the user config — an adapter's command is not
/// a project's to set.
const DEBUG_HEADER: &str = "\
# bi project config — debug launch configurations
#
# This file is a PATCH over your user config, read from the working
# directory (or the nearest parent that has one). Everything below is
# commented out: uncomment one whole block, `[[debug.launch]]` line included,
# and set `program`. Adapters themselves (`[debug.adapters.*]`) are configured
# in the user config — `bi config init` — never here.

";

/// The debug sample as a menu: every block commented out *whole*.
///
/// Not [`commented`]'s transform, which leaves table headers live: a live
/// `[[debug.launch]]` over commented keys is an empty entry the parser
/// rejects, so here the header is commented with its keys. The prose lines
/// are already comments and pass through.
fn debug_menu() -> String {
    let mut out = String::from(DEBUG_HEADER);
    for line in bi::config::DEBUG_SAMPLE.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            out.push_str(line);
        } else {
            out.push_str("# ");
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// What `bi debug init` found, for the caller to narrate.
enum DebugInit {
    Wrote(PathBuf),
    /// The file was there already — untouched. The caller prints the sample
    /// instead, so asking for one is never a no-op.
    Existed(PathBuf),
}

/// Writes `.bi.toml` in `dir` if it is absent. Never overwrites: a project's
/// config is the project's, and this seeds one only where there is none.
fn debug_init(dir: &Path) -> Result<DebugInit> {
    let path = dir.join(".bi.toml");
    if path.exists() {
        return Ok(DebugInit::Existed(path));
    }
    std::fs::write(&path, debug_menu()).with_context(|| format!("writing {}", path.display()))?;
    Ok(DebugInit::Wrote(path))
}

/// Whether the kitty keyboard protocol was pushed, so `restore` — reachable
/// from the panic hook, which captures nothing — knows to pop it.
static KITTY_KEYBOARD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn setup() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Bracketed paste is what makes a paste arrive as one event instead of as
    // one keystroke per character. Without it the terminal has no way to say
    // "this is a paste", and a 2 KB paste costs 2000 redraws.
    // Focus events feed checktime: coming back to the terminal is the moment
    // to notice a file that changed while you were away. A terminal that does
    // not send them costs the moment, nothing else — the poll still runs.
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, EnableFocusChange)?;

    // A legacy terminal sends `Enter` and `Ctrl-Enter` as the same byte, so
    // the chord only exists where the kitty keyboard protocol does. Asked,
    // never assumed, and only the mildest level — disambiguate changes how
    // ambiguous keys are encoded and nothing else. A `false` costs the
    // chord, not the key: `Enter` keeps arriving as `Enter` either way.
    // See docs/specs/open-in-split.md.
    if terminal::supports_keyboard_enhancement().unwrap_or(false) {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
        KITTY_KEYBOARD.store(true, std::sync::atomic::Ordering::Relaxed);
    }

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
        // Popped before raw mode goes, and only if it was pushed: a pop the
        // terminal never saw the push for would pop someone else's flags.
        if KITTY_KEYBOARD.swap(false, std::sync::atomic::Ordering::Relaxed) {
            execute!(io::stdout(), PopKeyboardEnhancementFlags)?;
        }
        disable_raw_mode()?;
        // Bracketed paste comes off here too, panic hook included: a terminal
        // left in it pastes escape noise into the next shell prompt.
        execute!(io::stdout(), DisableFocusChange, DisableBracketedPaste, LeaveAlternateScreen)?;
    }
    Ok(())
}

/// One thing the loop can be woken by. Terminal events arrive from a thread
/// that blocks in `event::read`; LSP wakes arrive from server reader threads.
/// One channel, so the loop blocks on exactly one thing — and every async
/// source there will ever be joins by sending into it.
enum Wake {
    Term(Event),
    Lsp,
    Dap,
    Shell,
}

fn run(term: &mut Term, ed: &mut Editor, gfx: &mut tui::graphics::Graphics) -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();

    // The terminal reader. Detached: it blocks in `event::read` with nothing
    // to interrupt it, and process exit is what ends it — the same blocking
    // read this loop itself used to sit in, moved one thread over so a
    // language server can wake the editor while the keyboard is silent.
    let term_tx = tx.clone();
    std::thread::spawn(move || {
        loop {
            match event::read() {
                Ok(event) => {
                    if term_tx.send(Wake::Term(event)).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });
    let dap_tx = tx.clone();
    let shell_tx = tx.clone();
    ed.set_lsp_waker(move || {
        let _ = tx.send(Wake::Lsp);
    });
    ed.set_dap_waker(move || {
        let _ = dap_tx.send(Wake::Dap);
    });
    // The `:!` job's reader and waiter threads wake the loop the same way.
    // See docs/specs/shell.md.
    ed.set_shell_waker(move || {
        let _ = shell_tx.send(Wake::Shell);
    });

    let mut input = Input::default();
    // The keymap lives on `Input`, which is the frontend's, while `:reload`
    // happens inside the editor. `config_epoch` is how the two meet: the
    // number changes, this notices, and the map is reinstalled. Cloning a map
    // of a dozen entries once per reload is not worth a subtler arrangement.
    let mut installed = None;

    loop {
        if installed != Some(ed.config_epoch()) {
            installed = Some(ed.config_epoch());
            input.set_keys(ed.config().keys.clone());
        }

        // Where every image pane wants pixels this frame, collected by the
        // renderer and brought to the screen after ratatui's own draw — the
        // escapes ride behind the frame, never through it.
        let cell = gfx.cell_size();
        let mut places = Vec::new();
        term.draw(|frame| {
            tui::render::render(frame, ed, &input.pending_display(), cell, &mut places)
        })?;
        gfx.sync(ed, &places)?;

        // Something on screen may go away on its own — the flash a yank left.
        // Waiting is the frontend's job and the clock is the editor's, so the
        // loop asks how long it may block for and draws when that runs out
        // whether or not anything arrived. `None` is the usual case: nothing
        // is pending, so `recv` blocks as `event::read` used to.
        let first = match ed.redraw_in() {
            Some(until) => match rx.recv_timeout(until) {
                Ok(wake) => wake,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            },
            None => match rx.recv() {
                Ok(wake) => wake,
                Err(_) => return Ok(()),
            },
        };

        // Everything that has already arrived is applied before the next draw.
        // A frame the user never sees is a frame not worth rendering, and this
        // is what makes a burst — a paste into a terminal that does not support
        // bracketed paste, or a held-down `j` — cost one redraw instead of one
        // per keystroke. The first wake blocked above; the loop then drains
        // whatever is queued behind it without waiting.
        let mut next = Some(first);
        while let Some(wake) = next {
            match wake {
                // Windows terminals emit Release too; without this filter every
                // key fires twice.
                Wake::Term(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    if let Some(key) = tui::keys::translate(key)
                        && let Some(cmd) = input.on_key(key, &ed.session.mode, ed.content_kind())
                    {
                        ed.session.status.clear();
                        ed.apply(cmd);
                    }
                }
                // A bracketed paste: one event, one insertion, one undo entry.
                // The terminal sends it whole, so nothing here has to guess
                // where it ends.
                Wake::Term(Event::Paste(text)) => ed.paste_text(text),
                // Coming back to the terminal is a checktime moment: files
                // change while you are away in another pane.
                Wake::Term(Event::FocusGained) => ed.focus_gained(),
                Wake::Term(_) => {}
                // Nothing to do here by name: the settle below pumps the LSP
                // inbox along with everything else.
                Wake::Lsp => {}
                // Same story for DAP — the settle below pumps its inbox too.
                Wake::Dap => {}
                // Same story for a `:!` job — the settle below pumps its
                // slot too.
                Wake::Shell => {}
            }
            // Feed the parse tree and the language servers — both hang off
            // this one drain.
            //
            // Per event rather than once per burst, even though once would be
            // cheaper: `settle` skips the focused window because the command
            // that moved the text moved its cursor too, and a burst that
            // changes focus would apply that skip to the wrong window.
            ed.settle();
            if ed.session.quit {
                return Ok(());
            }
            next = rx.try_recv().ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_route_the_two_subcommands_and_nothing_else() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        assert!(matches!(parse_args(&args(&[])).unwrap(), Invocation::Open(None)));
        assert!(matches!(parse_args(&args(&["a.rs"])).unwrap(), Invocation::Open(Some(_))));
        assert!(
            matches!(parse_args(&args(&["config"])).unwrap(), Invocation::Open(Some(_))),
            "a file named `config` still opens; the subcommand form takes two words"
        );
        assert!(matches!(parse_args(&args(&["config", "init"])).unwrap(), Invocation::ConfigInit));
        assert!(matches!(parse_args(&args(&["config", "edit"])).unwrap(), Invocation::ConfigEdit));
        assert!(parse_args(&args(&["config", "nope"])).is_err());
        assert!(matches!(parse_args(&args(&["debug", "init"])).unwrap(), Invocation::DebugInit));
        assert!(parse_args(&args(&["debug", "nope"])).is_err());
        assert!(parse_args(&args(&["a.rs", "b.rs"])).is_err());
    }

    #[test]
    fn debug_init_writes_once_then_reports_the_existing_file() {
        let dir = std::env::temp_dir().join(format!("bi-debug-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let first = debug_init(&dir).unwrap();
        assert!(matches!(first, DebugInit::Wrote(_)));
        let written = std::fs::read_to_string(dir.join(".bi.toml")).unwrap();
        assert!(written.contains("[[debug.launch]]"), "{written}");

        std::fs::write(dir.join(".bi.toml"), "mine\n").unwrap();
        let second = debug_init(&dir).unwrap();
        assert!(matches!(second, DebugInit::Existed(_)), "the file is reported, not replaced");
        assert_eq!(std::fs::read_to_string(dir.join(".bi.toml")).unwrap(), "mine\n");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_debug_menu_has_no_live_line() {
        // Whole blocks commented, headers included: an uncommented
        // `[[debug.launch]]` over commented keys would be an empty entry the
        // parser rejects, not a menu.
        let menu = debug_menu();
        for line in menu.lines() {
            assert!(
                line.trim().is_empty() || line.starts_with('#'),
                "live line in the menu: {line:?}"
            );
        }
        assert!(menu.contains("# [[debug.launch]]"), "{menu}");
    }

    #[test]
    fn the_written_config_is_the_defaults_commented_out() {
        let out = commented("[options]\nnumber = 1\n\n# already a comment\n");

        // Section headers are written live: an empty table is as inert as no
        // table at all, and this is what lets uncommenting a single setting
        // line actually work.
        assert!(out.contains("\n[options]\n"), "the header is live: {out}");
        assert!(!out.contains("# [options]"), "not commented: {out}");
        assert!(out.contains("# number = 1"));
        assert!(out.contains("# already a comment"), "and not double-commented");
        assert!(!out.contains("## already"), "{out}");

        // The whole file must be inert, or a user's config silently becomes a
        // full replacement and they stop receiving later defaults.
        let (config, problems) = bi::config::parse(&out, bi::config::Config::default()).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(config, bi::config::Config::default(), "semantically empty");
    }

    #[test]
    fn the_real_shipped_defaults_commented_out_are_still_semantically_empty() {
        // The synthetic test above pins the line-shape mechanics; this one
        // exercises the actual file `bi config init` writes, which is what
        // the review that found the section-header trap said was missing.
        let out = commented(&defaults());

        let (config, problems) = bi::config::parse(&out, bi::config::Config::default()).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(config, bi::config::Config::default(), "semantically empty");
    }

    /// The point of writing the keymap out: every binding bi has is in the
    /// file, so the answer to "what can I rebind, and to what?" is the file
    /// itself rather than the source.
    #[test]
    fn the_written_config_lists_every_binding_and_every_one_of_them_parses() {
        let out = commented(&defaults());

        for expected in [
            "[keys.normal]",
            "[keys.visual]",
            "[keys.tree]",
            "# \"h\"",
            "= \"left\"",
            "= \"goto_first_line\"",
            "= \"window_tree\"",
            "= \"tree_delete\"",
            "# leader = \" \"",
        ] {
            assert!(out.contains(expected), "missing {expected}:\n{out}");
        }

        // Uncommenting the lot must be a keymap that binds every key to what it
        // already does — which is what makes the listing a menu rather than
        // decoration. Anything the generator got wrong shows up here as a
        // diagnostic instead of in a user's config file.
        let live: String = out
            .lines()
            .map(|line| line.strip_prefix("# ").filter(|l| l.contains(" = ")).unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");
        let (_, problems) =
            bi::config::parse(&live, bi::config::Config::default()).expect("it is still TOML");
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn uncommenting_exactly_one_setting_line_just_works() {
        // The header promises "uncomment a line only to change it" — this
        // pins that promise against the real shipped file, the way a user
        // actually edits it: find the line, strip its leading `# `, touch
        // nothing else. With the section header still commented out, this
        // used to leave the key belonging to no section and produce exactly
        // one diagnostic instead of zero.
        let out = commented(bi::config::DEFAULT_TOML);
        let uncommented: String = out
            .lines()
            .map(
                |line| {
                    if line == "# number = 1" { line.strip_prefix("# ").unwrap() } else { line }
                },
            )
            .collect::<Vec<_>>()
            .join("\n");

        assert_ne!(uncommented, out, "the line was actually uncommented");

        let (config, problems) =
            bi::config::parse(&uncommented, bi::config::Config::default()).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(config.options.number, bi::editor::LineNumbers::Every(1));
    }

    #[test]
    fn init_writes_once_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("bi-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        config_init(&dir).unwrap();
        let first = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        assert!(first.contains("PATCH over bi's defaults"));

        std::fs::write(dir.join("config.toml"), "mine\n").unwrap();
        config_init(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("config.toml")).unwrap(),
            "mine\n",
            "a second init leaves the user's file alone"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edit_refuses_a_directory_that_does_not_exist() {
        let missing = std::env::temp_dir().join(format!("bi-absent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);

        let err = config_edit_path(&missing).expect_err("nothing to edit yet");
        assert!(err.to_string().contains("bi config init"), "{err}");
    }

    #[test]
    fn edit_opens_the_directory_it_finds() {
        let dir = std::env::temp_dir().join(format!("bi-edit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        config_init(&dir).unwrap();

        assert_eq!(
            config_edit_path(&dir).unwrap(),
            dir,
            "the directory, so themes/ is in the tree"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
