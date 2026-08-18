//! bi's terminal frontend.
//!
//! Terminal setup, the event loop, and nothing else. The editor itself is the
//! `bi` library — see `src/lib.rs`. A second frontend would replace this file
//! and `src/tui/`, and touch nothing below them.

mod tui;

use std::io::{self, Stdout};
use std::panic;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind,
};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::crossterm::{execute, terminal};

use bi::config::ConfigSource;
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
        Invocation::Open(path) => path,
    };

    let mut editor = match path {
        Some(path) => Editor::open(path)?,
        None => Editor::empty(),
    };

    let problems = editor.load_config(XdgConfig { dir: config_dir() });
    // `"+y` and `"+p`. The library holds the trait; the terminal is what knows
    // how to reach a clipboard from inside one.
    editor.set_clipboard(tui::clipboard::Osc52);

    let mut term = setup().context("entering raw mode")?;

    if !problems.is_empty() {
        let n = problems.len();
        editor.session.status =
            format!("{n} config problem{}: {}", if n == 1 { "" } else { "s" }, problems[0].message);
    }

    let result = run(&mut term, &mut editor);
    restore()?;
    result
}

/// What the command line asked for.
enum Invocation {
    Open(Option<String>),
    ConfigInit,
    ConfigEdit,
}

/// `config` is a subcommand only in the two-word form, so a file actually
/// named `config` still opens.
fn parse_args(args: &[String]) -> Result<Invocation> {
    match args {
        [] => Ok(Invocation::Open(None)),
        [one] => Ok(Invocation::Open(Some(one.clone()))),
        [first, sub] if first == "config" => match sub.as_str() {
            "init" => Ok(Invocation::ConfigInit),
            "edit" => Ok(Invocation::ConfigEdit),
            other => bail!("no such command: bi config {other} — try `init` or `edit`"),
        },
        _ => bail!("usage: bi [path] | bi config init | bi config edit"),
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

/// bi's config directory: `$BI_CONFIG`, else `$XDG_CONFIG_HOME/bi`, else
/// `~/.config/bi`.
///
/// A directory rather than a file, because `themes/` is its sibling and
/// `bi config edit` opens the lot. This is the whole of what the frontend
/// knows that the library does not.
fn config_dir() -> Option<PathBuf> {
    dir_from(
        std::env::var("BI_CONFIG").ok().as_deref(),
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// The rule, with the environment passed in so it can be tested without
/// setting process-wide variables — which two tests running at once would
/// fight over.
fn dir_from(bi: Option<&str>, xdg: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    fn some(s: Option<&str>) -> Option<&str> {
        s.filter(|s| !s.is_empty())
    }

    if let Some(explicit) = some(bi) {
        return Some(PathBuf::from(explicit));
    }
    if let Some(xdg) = some(xdg) {
        return Some(PathBuf::from(xdg).join("bi"));
    }
    some(home).map(|home| PathBuf::from(home).join(".config").join("bi"))
}

/// Reads bi's config off the filesystem.
struct XdgConfig {
    dir: Option<PathBuf>,
}

impl XdgConfig {
    /// The shared body of both reads: a missing file is the normal case and
    /// not a problem, an unreadable one is.
    fn read(&self, relative: &Path) -> Result<Option<String>> {
        let Some(dir) = &self.dir else { return Ok(None) };
        let path = dir.join(relative);
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context(format!("reading {}", path.display())),
        }
    }
}

impl ConfigSource for XdgConfig {
    fn config(&self) -> Result<Option<String>> {
        self.read(Path::new("config.toml"))
    }

    /// `themes/<name>.toml`, which is why the config location is a directory
    /// rather than a file. A name with a separator in it would reach outside
    /// that directory, so it does not get to: a theme is one file beside the
    /// config, not a path.
    fn theme(&self, name: &str) -> Result<Option<String>> {
        if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") {
            return Ok(None);
        }
        self.read(&Path::new("themes").join(format!("{name}.toml")))
    }
}

fn setup() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Bracketed paste is what makes a paste arrive as one event instead of as
    // one keystroke per character. Without it the terminal has no way to say
    // "this is a paste", and a 2 KB paste costs 2000 redraws.
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;

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
        // Bracketed paste comes off here too, panic hook included: a terminal
        // left in it pastes escape noise into the next shell prompt.
        execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen)?;
    }
    Ok(())
}

fn run(term: &mut Term, ed: &mut Editor) -> Result<()> {
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

        term.draw(|frame| tui::render::render(frame, ed, &input.pending_display()))?;

        // Everything that has already arrived is applied before the next draw.
        // A frame the user never sees is a frame not worth rendering, and this
        // is what makes a burst — a paste into a terminal that does not support
        // bracketed paste, or a held-down `j` — cost one redraw instead of one
        // per keystroke. The first `read` blocks; the loop then drains whatever
        // is queued behind it without waiting.
        let mut pending = true;
        while pending {
            match event::read()? {
                // Windows terminals emit Release too; without this filter every
                // key fires twice.
                Event::Key(key) if key.kind == KeyEventKind::Press => {
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
                Event::Paste(text) => ed.paste_text(text),
                Event::Resize(_, _) => {}
                _ => {}
            }
            // Feed the parse tree. LSP will hang off the same drain.
            //
            // Per event rather than once per burst, even though once would be
            // cheaper: `settle` skips the focused window because the command
            // that moved the text moved its cursor too, and a burst that
            // changes focus would apply that skip to the wrong window.
            ed.settle();
            if ed.session.quit {
                return Ok(());
            }
            pending = event::poll(Duration::ZERO)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A theme name reaches the filesystem, so it does not get to be a
    /// path. `../../etc/passwd` is not a theme.
    #[test]
    fn a_theme_name_cannot_escape_the_themes_directory() {
        let source = XdgConfig { dir: Some(PathBuf::from("/nonexistent")) };
        for escape in ["../secrets", "..", "a/b", "a\\b", ""] {
            assert!(
                source.theme(escape).unwrap().is_none(),
                "{escape:?} should not have been looked up at all"
            );
        }
    }

    #[test]
    fn config_dir_prefers_bi_config_then_xdg_then_home() {
        let bi = dir_from(Some("/explicit"), Some("/xdg"), Some("/home"));
        assert_eq!(bi, Some(PathBuf::from("/explicit")));

        let xdg = dir_from(None, Some("/xdg"), Some("/home"));
        assert_eq!(xdg, Some(PathBuf::from("/xdg/bi")));

        let home = dir_from(None, None, Some("/home"));
        assert_eq!(home, Some(PathBuf::from("/home/.config/bi")));

        assert_eq!(dir_from(None, None, None), None, "nowhere to look is not a crash");
    }

    #[test]
    fn an_empty_env_var_is_the_same_as_an_unset_one() {
        assert_eq!(
            dir_from(Some(""), None, Some("/home")),
            Some(PathBuf::from("/home/.config/bi"))
        );
    }

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
        assert!(parse_args(&args(&["a.rs", "b.rs"])).is_err());
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
    fn xdg_config_reports_no_dir_as_no_config() {
        // `dir: None` is what `config_dir()` returns when neither
        // `$BI_CONFIG`, `$XDG_CONFIG_HOME` nor `$HOME` is set — nowhere to
        // look is not an error, it is the normal case for `ConfigSource`.
        let source = XdgConfig { dir: None };
        assert_eq!(source.config().unwrap(), None);
    }

    #[test]
    fn xdg_config_reports_a_dir_with_no_file_as_no_config() {
        let dir = std::env::temp_dir().join(format!("bi-xdg-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let source = XdgConfig { dir: Some(dir.clone()) };
        assert_eq!(source.config().unwrap(), None, "the NotFound -> Ok(None) rule");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn xdg_config_reads_a_written_file() {
        let dir = std::env::temp_dir().join(format!("bi-xdg-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), "[options]\nnumber = 5\n").unwrap();

        let source = XdgConfig { dir: Some(dir.clone()) };
        assert_eq!(source.config().unwrap(), Some("[options]\nnumber = 5\n".to_string()));

        std::fs::remove_dir_all(&dir).unwrap();
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
