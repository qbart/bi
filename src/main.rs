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

use bi::config::ConfigSource;
use bi::editor::Editor;
use bi::input::Input;

type Term = Terminal<CrosstermBackend<Stdout>>;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let path = match parse_args(&args)? {
        Invocation::Help(text) => {
            print!("{text}");
            return Ok(());
        }
        Invocation::Version => {
            println!("bi {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
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
        Invocation::GenSample(kind) => {
            let dir = std::env::current_dir().context("no working directory to write into")?;
            for outcome in gen_sample(&dir, kind)? {
                match outcome {
                    Generated::Wrote(path) => println!("wrote {}", path.display()),
                    // As `debug init`: the text still reaches stdout, the
                    // warning stderr, so a redirect stays clean.
                    Generated::Existed(path, text) => {
                        print!("{text}");
                        eprintln!(
                            "{} already exists — printed the sample instead of creating it",
                            path.display()
                        );
                    }
                }
            }
            return Ok(());
        }
        Invocation::GenStruct(args) => return gen_struct(&args),
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
    let problems = editor.load_config(XdgConfig { dir: config_dir() });
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

/// What the command line asked for. See `docs/specs/cli.md`.
#[derive(Debug, PartialEq)]
enum Invocation {
    Open(Option<String>),
    /// `--help`, or a command word alone: the list to print.
    Help(&'static str),
    Version,
    ConfigInit,
    ConfigEdit,
    /// `bi debug init` — a project's `.bi.toml` seeded with launch configs.
    DebugInit,
    /// `bi gen sample [schema|data|mapping]` — the sample `.bischema`,
    /// `.bidata` and `.bimapping` written beside you. See
    /// `docs/specs/props.md`.
    GenSample(Option<Sample>),
    /// `bi gen struct …` — the schema as code. See `docs/specs/gen-struct.md`.
    GenStruct(GenStructArgs),
}

/// One of the three sample files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sample {
    Schema,
    Data,
    Mapping,
}

impl Sample {
    const ALL: [Sample; 3] = [Sample::Schema, Sample::Data, Sample::Mapping];

    fn parse(word: &str) -> Option<Sample> {
        Some(match word {
            "schema" | "bischema" => Sample::Schema,
            "data" | "bidata" => Sample::Data,
            "mapping" | "bimapping" => Sample::Mapping,
            _ => return None,
        })
    }

    fn file(self) -> (&'static str, &'static str) {
        use bi::props::sample;
        match self {
            Sample::Schema => (sample::SCHEMA_NAME, sample::SCHEMA),
            Sample::Data => (sample::DATA_NAME, sample::DATA),
            Sample::Mapping => (sample::MAPPING_NAME, sample::MAPPING),
        }
    }
}

/// The flags of `bi gen struct`, parsed and nothing more.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GenStructArgs {
    lang: bi::codegen::structs::Lang,
    out: PathBuf,
    inputs: Vec<PathBuf>,
    mappings: Vec<PathBuf>,
    pkg: Option<String>,
    force: bool,
    verbose: bool,
}

const USAGE: &str = "\
bi — a text editor

usage:
  bi                             start empty
  bi <path>                      open the file
  bi -- <path>                   open the file, whatever it is called
  bi config init                 the user config written, every default commented out
  bi config edit                 the user config opened, written first when it is not there
  bi debug init                  a project's .bi.toml seeded with launch configs
  bi gen sample [schema|data|mapping]
                                 the sample .bischema, .bidata and .bimapping written beside you
  bi gen struct --lang <lang> -o <dir> -i <file>... [-m <file>...] [--pkg <name>] [--force] [-v]
                                 the schema and its data as C, C++, Go, Rust, C3 or Lua
  bi help <command>              one command's list
  bi --help, -h                  this list
  bi --version, -V               the version

A command word wins over a file of the same name: `bi ./gen` or `bi -- gen`
opens the file.
";

const CONFIG_USAGE: &str = "\
usage: bi config <command>

  init    the user config written, every default commented out
  edit    the user config opened, written first when it is not there
";

const DEBUG_USAGE: &str = "\
usage: bi debug <command>

  init    a project's .bi.toml seeded with launch configs
";

const GEN_USAGE: &str = "\
usage: bi gen <command>

  sample [schema|data|mapping]
      the sample .bischema, .bidata and .bimapping written beside you;
      a kind for one of them

  struct --lang <lang> -o <dir> -i <file>... [-m <file>...] [--pkg <name>] [--force] [-v]
      the schema's types, defaults and ids, and the data's instances, as code
        --lang <lang>   c | c++ | go | rust | c3 | lua
        -o <dir>        where the files go; made when missing
        -i <file>       a .bischema or .bidata; a data file brings its schema
        -m <file>       a .bimapping; later files win on the keys they share
        --pkg <name>    the namespace, package or module (a.b)
        --force         overwrite without asking
        -v              say what was decided
";

/// The list a command word prints on its own, or under `help`.
fn usage_of(word: &str) -> Option<&'static str> {
    Some(match word {
        "config" => CONFIG_USAGE,
        "debug" => DEBUG_USAGE,
        "gen" => GEN_USAGE,
        _ => return None,
    })
}

/// A command word wins over a file of the same name; `--` or a `./` puts
/// the file back. See `docs/specs/cli.md`.
fn parse_args(args: &[String]) -> Result<Invocation> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] | ["--"] => Ok(Invocation::Open(None)),
        ["--", path] => Ok(Invocation::Open(Some((*path).to_string()))),
        ["--help" | "-h" | "help"] => Ok(Invocation::Help(USAGE)),
        ["help", word] => match usage_of(word) {
            Some(text) => Ok(Invocation::Help(text)),
            None => bail!("no such command: bi {word} — `bi --help` lists them"),
        },
        ["--version" | "-V"] => Ok(Invocation::Version),
        [word] if usage_of(word).is_some() => Ok(Invocation::Help(usage_of(word).unwrap())),
        ["config", sub] => match *sub {
            "init" => Ok(Invocation::ConfigInit),
            "edit" => Ok(Invocation::ConfigEdit),
            other => bail!("no such command: bi config {other} — try `init` or `edit`"),
        },
        ["debug", sub] => match *sub {
            "init" => Ok(Invocation::DebugInit),
            other => bail!("no such command: bi debug {other} — try `init`"),
        },
        ["gen", sub, rest @ ..] => match (*sub, rest) {
            ("sample", []) => Ok(Invocation::GenSample(None)),
            ("sample", [kind]) => match Sample::parse(kind) {
                Some(kind) => Ok(Invocation::GenSample(Some(kind))),
                None => bail!("no such sample: {kind} — try `schema`, `data` or `mapping`"),
            },
            ("sample", _) => bail!("usage: bi gen sample [schema|data|mapping]"),
            ("struct", flags) => Ok(Invocation::GenStruct(parse_gen_struct(flags)?)),
            (other, _) => bail!("no such command: bi gen {other} — try `sample` or `struct`"),
        },
        [flag] if flag.starts_with('-') => {
            bail!("no such flag: {flag} — `bi --help` lists them, `bi -- {flag}` opens the file")
        }
        [one] => Ok(Invocation::Open(Some((*one).to_string()))),
        _ => bail!("usage: bi [path] — `bi --help` lists the commands"),
    }
}

/// The flags of `bi gen struct`, in any order; a value that starts with
/// `-` is a missing value, `--` ends the flags.
fn parse_gen_struct(flags: &[&str]) -> Result<GenStructArgs> {
    use bi::codegen::structs::Lang;
    let mut lang = None;
    let mut out = None;
    let mut inputs = Vec::new();
    let mut mappings = Vec::new();
    let mut pkg = None;
    let mut force = false;
    let mut verbose = false;
    let mut i = 0;
    let mut literal = false;
    while i < flags.len() {
        let flag = flags[i];
        i += 1;
        if literal {
            inputs.push(PathBuf::from(flag));
            continue;
        }
        let mut value = |what: &str| -> Result<&str> {
            match flags.get(i) {
                Some(v) if !v.starts_with('-') || *v == "-" => {
                    i += 1;
                    Ok(v)
                }
                Some(v) => bail!("{flag} wants {what}, got {v}"),
                None => bail!("{flag} wants {what}"),
            }
        };
        match flag {
            "--" => literal = true,
            "--lang" => {
                let v = value("a language")?;
                if lang.is_some() {
                    bail!("--lang given twice — one language per run");
                }
                lang = Some(Lang::parse(v).ok_or_else(|| {
                    anyhow::anyhow!("no such language: {v} — {}", Lang::SPELLINGS)
                })?);
            }
            "-o" | "--out" => {
                let v = value("a directory")?;
                if out.is_some() {
                    bail!("-o given twice");
                }
                out = Some(PathBuf::from(v));
            }
            "-i" | "--input" => inputs.push(PathBuf::from(value("a file")?)),
            "-m" | "--mapping" => mappings.push(PathBuf::from(value("a file")?)),
            "--pkg" => {
                let v = value("a name")?;
                pkg = Some(v.to_string());
            }
            "--force" | "-f" => force = true,
            "-v" | "--verbose" => verbose = true,
            other if other.starts_with('-') => {
                bail!("no such flag: {other} — `bi help gen` lists them")
            }
            other => inputs.push(PathBuf::from(other)),
        }
    }
    let Some(lang) = lang else {
        bail!("bi gen struct: --lang <lang> is required — {}", Lang::SPELLINGS)
    };
    let Some(out) = out else { bail!("bi gen struct: -o <dir> is required") };
    if inputs.is_empty() {
        bail!("bi gen struct: -i <file> is required");
    }
    Ok(GenStructArgs { lang, out, inputs, mappings, pkg, force, verbose })
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
/// One file `bi gen sample` handled.
enum Generated {
    Wrote(PathBuf),
    Existed(PathBuf, &'static str),
}

/// `bi gen sample [schema|data|mapping]`: `game.bischema`, `level1.bidata`
/// and `game.bimapping` written into `dir` — all three when no kind is
/// named — each left alone when it already exists. The data file points
/// at the schema by name, so the pair opens straight into the property
/// view, and the mapping is every key commented out, so `bi gen struct`
/// over them generates as written.
fn gen_sample(dir: &Path, kind: Option<Sample>) -> Result<Vec<Generated>> {
    let wanted: Vec<Sample> = match kind {
        Some(kind) => vec![kind],
        None => Sample::ALL.to_vec(),
    };
    let mut out = Vec::new();
    for kind in wanted {
        let (name, text) = kind.file();
        let path = dir.join(name);
        if path.exists() {
            out.push(Generated::Existed(path, text));
            continue;
        }
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        out.push(Generated::Wrote(path));
    }
    Ok(out)
}

/// `bi gen struct`: the files read, the library asked, the answer written
/// under the policy — the terminal's question, or `--force`'s yes, or a
/// pipe's no. Errors go to stderr and fail the run; nothing is written
/// when anything is wrong.
fn gen_struct(args: &GenStructArgs) -> Result<()> {
    use bi::codegen::structs::{self, Always, Ask, Overwrite, Request};
    use std::io::IsTerminal;

    let read_all = |paths: &[PathBuf]| -> Result<Vec<(PathBuf, String)>> {
        paths
            .iter()
            .map(|p| {
                std::fs::read_to_string(p)
                    .map(|t| (p.clone(), t))
                    .with_context(|| format!("reading {}", p.display()))
            })
            .collect()
    };
    let dir_name = args
        .out
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
        })
        .unwrap_or_else(|| "gen".to_string());
    let req = Request {
        lang: args.lang,
        pkg: args.pkg.clone(),
        inputs: read_all(&args.inputs)?,
        mappings: read_all(&args.mappings)?,
        dir_name,
    };
    let mut read = |p: &Path| std::fs::read_to_string(p).map_err(|e| e.to_string());
    let output = match structs::generate(&req, &mut read) {
        Ok(out) => out,
        Err(errors) => {
            for e in &errors {
                eprintln!("{}", e.message);
            }
            bail!(
                "bi gen struct: {} error{}",
                errors.len(),
                if errors.len() == 1 { "" } else { "s" }
            );
        }
    };
    for note in &output.notes {
        let level = match note.level {
            bi::props::Level::Error => "error",
            bi::props::Level::Warning => "note",
        };
        if args.verbose || note.level == bi::props::Level::Error {
            eprintln!("{level}: {}", note.message);
        }
    }
    if !args.verbose && !output.notes.is_empty() {
        eprintln!(
            "{} note{} — -v shows them",
            output.notes.len(),
            if output.notes.len() == 1 { "" } else { "s" }
        );
    }
    if args.out.is_file() {
        bail!("-o {}: a file, not a directory", args.out.display());
    }

    /// Reads one answer from the terminal per differing file.
    struct TerminalAsk;
    impl Ask for TerminalAsk {
        fn overwrite(&mut self, path: &Path) -> Overwrite {
            loop {
                eprint!("overwrite {}? [y/N/a/q] ", path.display());
                let mut line = String::new();
                if io::stdin().read_line(&mut line).is_err() {
                    return Overwrite::Quit;
                }
                match line.trim().to_ascii_lowercase().as_str() {
                    "y" | "yes" => return Overwrite::Yes,
                    "" | "n" | "no" => return Overwrite::No,
                    "a" | "all" => return Overwrite::All,
                    "q" | "quit" => return Overwrite::Quit,
                    _ => eprintln!("y — this one; n — not this one; a — all of them; q — stop"),
                }
            }
        }
    }

    let tty = io::stdin().is_terminal();
    let mut force = Always(Overwrite::All);
    let mut refuse = Always(Overwrite::No);
    let mut terminal = TerminalAsk;
    let ask: &mut dyn Ask = if args.force {
        &mut force
    } else if tty {
        &mut terminal
    } else {
        &mut refuse
    };
    let report = structs::write(&args.out, &output.files, ask)
        .with_context(|| format!("writing under {}", args.out.display()))?;
    for (path, fate) in &report.fates {
        let tail = match fate {
            structs::Fate::Skipped if !args.force && !tty => " — --force to overwrite",
            _ => "",
        };
        if args.verbose || *fate != structs::Fate::Unchanged {
            println!("{} {}{tail}", fate.word(), path.display());
        }
    }
    if report.stopped {
        bail!("stopped — the files after it were not written");
    }
    if report.skipped() {
        bail!("some files were not written");
    }
    Ok(())
}

fn debug_init(dir: &Path) -> Result<DebugInit> {
    let path = dir.join(".bi.toml");
    if path.exists() {
        return Ok(DebugInit::Existed(path));
    }
    std::fs::write(&path, debug_menu()).with_context(|| format!("writing {}", path.display()))?;
    Ok(DebugInit::Wrote(path))
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

/// The nearest `.bi.toml`, walking up from `start` — one file, first found
/// wins, no layering of several. Every kind of lookup trouble is `None`:
/// missing, unreadable, a directory this process may not enter — the
/// `ConfigSource::local` contract, spelled here as "any failed read is just
/// the next parent's turn". See `docs/specs/local-config.md`.
fn local_config_from(start: &Path) -> Option<(PathBuf, String)> {
    for dir in start.ancestors() {
        let path = dir.join(".bi.toml");
        if let Ok(text) = std::fs::read_to_string(&path) {
            return Some((path, text));
        }
    }
    None
}

impl ConfigSource for XdgConfig {
    fn config(&self) -> Result<Option<String>> {
        self.read(Path::new("config.toml"))
    }

    /// From the working directory: where bi was started is the project it is
    /// in — process state, which is the frontend's to know.
    fn local(&self) -> Option<(PathBuf, String)> {
        local_config_from(&std::env::current_dir().ok()?)
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

    /// See `docs/specs/cli.md`.
    #[test]
    fn args_route_the_commands_and_a_command_word_wins_over_a_file() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        assert!(matches!(parse_args(&args(&[])).unwrap(), Invocation::Open(None)));
        assert!(matches!(parse_args(&args(&["a.rs"])).unwrap(), Invocation::Open(Some(_))));
        for flag in ["--help", "-h", "help"] {
            assert!(
                matches!(parse_args(&args(&[flag])).unwrap(), Invocation::Help(USAGE)),
                "{flag}"
            );
        }
        assert!(matches!(parse_args(&args(&["--version"])).unwrap(), Invocation::Version));
        assert!(matches!(parse_args(&args(&["-V"])).unwrap(), Invocation::Version));
        for word in ["config", "debug", "gen"] {
            let alone = parse_args(&args(&[word])).unwrap();
            let Invocation::Help(text) = alone else { panic!("bi {word} lists its commands") };
            assert!(text.starts_with(&format!("usage: bi {word} <command>")), "{text}");
            let helped = parse_args(&args(&["help", word])).unwrap();
            assert!(matches!(helped, Invocation::Help(t) if t == text), "bi help {word}");
        }
        assert!(parse_args(&args(&["help", "nope"])).is_err());
        assert!(
            matches!(parse_args(&args(&["--", "gen"])).unwrap(), Invocation::Open(Some(p)) if p == "gen"),
            "`--` puts the file back"
        );
        assert!(
            matches!(parse_args(&args(&["./gen"])).unwrap(), Invocation::Open(Some(p)) if p == "./gen")
        );
        assert!(matches!(parse_args(&args(&["--"])).unwrap(), Invocation::Open(None)));
        let err = parse_args(&args(&["--nope"])).unwrap_err().to_string();
        assert!(err.contains("bi --help") && err.contains("bi -- --nope"), "{err}");
        assert!(USAGE.contains("bi gen sample [schema|data|mapping]"));
        assert!(USAGE.contains("bi gen struct --lang"));
        assert!(GEN_USAGE.contains("sample [schema|data|mapping]"));
        assert!(GEN_USAGE.contains("struct --lang"));
        assert!(matches!(parse_args(&args(&["config", "init"])).unwrap(), Invocation::ConfigInit));
        assert!(matches!(parse_args(&args(&["config", "edit"])).unwrap(), Invocation::ConfigEdit));
        assert!(parse_args(&args(&["config", "nope"])).is_err());
        assert!(matches!(parse_args(&args(&["debug", "init"])).unwrap(), Invocation::DebugInit));
        assert!(parse_args(&args(&["debug", "nope"])).is_err());
        assert!(matches!(
            parse_args(&args(&["gen", "sample"])).unwrap(),
            Invocation::GenSample(None)
        ));
        assert!(matches!(
            parse_args(&args(&["gen", "sample", "schema"])).unwrap(),
            Invocation::GenSample(Some(Sample::Schema))
        ));
        assert!(matches!(
            parse_args(&args(&["gen", "sample", "data"])).unwrap(),
            Invocation::GenSample(Some(Sample::Data))
        ));
        assert!(matches!(
            parse_args(&args(&["gen", "sample", "mapping"])).unwrap(),
            Invocation::GenSample(Some(Sample::Mapping))
        ));
        assert!(parse_args(&args(&["gen", "sample", "nope"])).is_err());
        assert!(parse_args(&args(&["gen", "nope"])).is_err());
        assert!(parse_args(&args(&["a.rs", "b.rs"])).is_err());
    }

    #[test]
    fn gen_struct_flags() {
        use bi::codegen::structs::Lang;
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let parsed = |list: &[&str]| {
            let mut v = vec!["gen", "struct"];
            v.extend_from_slice(list);
            parse_args(&args(&v))
        };
        let ok = parsed(&[
            "--lang",
            "rust",
            "-o",
            "out",
            "-i",
            "a.bischema",
            "-i",
            "b.bidata",
            "-m",
            "x.bimapping",
            "--pkg",
            "gen.so",
            "--force",
            "-v",
        ])
        .unwrap();
        assert_eq!(
            ok,
            Invocation::GenStruct(GenStructArgs {
                lang: Lang::Rust,
                out: PathBuf::from("out"),
                inputs: vec![PathBuf::from("a.bischema"), PathBuf::from("b.bidata")],
                mappings: vec![PathBuf::from("x.bimapping")],
                pkg: Some("gen.so".into()),
                force: true,
                verbose: true,
            })
        );
        // any order, c++ spelled so, a bare word is an input
        assert!(matches!(
            parsed(&["-i", "a.bischema", "--lang", "c++", "-o", "out"]).unwrap(),
            Invocation::GenStruct(GenStructArgs { lang: Lang::Cpp, .. })
        ));
        let e = |list: &[&str]| parsed(list).unwrap_err().to_string();
        assert_eq!(e(&["--lang", "rust", "-o", "out"]), "bi gen struct: -i <file> is required");
        assert_eq!(e(&["--lang", "rust", "-i", "a"]), "bi gen struct: -o <dir> is required");
        assert!(
            e(&["-o", "out", "-i", "a"]).starts_with("bi gen struct: --lang <lang> is required")
        );
        assert_eq!(
            e(&["--lang", "rust", "--lang", "go", "-o", "o", "-i", "a"]),
            "--lang given twice — one language per run"
        );
        assert_eq!(
            e(&["--lang", "java", "-o", "o", "-i", "a"]),
            "no such language: java — c, c++, go, rust, c3, lua"
        );
        assert_eq!(e(&["--lang", "rust", "-o", "-i", "a"]), "-o wants a directory, got -i");
        assert_eq!(e(&["--lang", "rust", "-o", "out", "-i"]), "-i wants a file");
        assert!(
            e(&["--lang", "rust", "-o", "out", "-i", "a", "--nope"])
                .starts_with("no such flag: --nope")
        );
        assert!(
            parse_args(&args(&["gen"]))
                .is_ok_and(|i| matches!(i, Invocation::Help(t) if t.contains("struct")))
        );
    }

    #[test]
    fn gen_sample_writes_the_pair_once_then_prints() {
        let dir = std::env::temp_dir().join(format!("bi-gen-sample-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let first = gen_sample(&dir, None).unwrap();
        assert!(matches!(
            first.as_slice(),
            [Generated::Wrote(_), Generated::Wrote(_), Generated::Wrote(_)]
        ));
        assert_eq!(
            std::fs::read_to_string(dir.join("game.bimapping")).unwrap(),
            bi::props::sample::MAPPING
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("game.bischema")).unwrap(),
            bi::props::sample::SCHEMA
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("level1.bidata")).unwrap(),
            bi::props::sample::DATA
        );
        let again = gen_sample(&dir, Some(Sample::Schema)).unwrap();
        assert!(
            matches!(again.as_slice(), [Generated::Existed(_, text)] if *text == bi::props::sample::SCHEMA)
        );
        // The pair opens straight into the view, refs resolving.
        let ed = Editor::open(dir.join("level1.bidata")).unwrap();
        let props = ed.window().props().expect("the property view");
        assert_eq!(props.error, None);
        assert_eq!(props.warnings(), 0);
        let _ = std::fs::remove_dir_all(&dir);
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
    fn the_local_config_walk_takes_the_nearest_and_swallows_all_lookup_trouble() {
        let dir = std::env::temp_dir().join(format!("bi-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("outer/inner/deep")).unwrap();
        std::fs::write(dir.join("outer/.bi.toml"), "outer").unwrap();
        std::fs::write(dir.join("outer/inner/.bi.toml"), "inner").unwrap();

        // The nearest one wins — no layering of several.
        let (path, text) = local_config_from(&dir.join("outer/inner/deep")).unwrap();
        assert_eq!(path, dir.join("outer/inner/.bi.toml"));
        assert_eq!(text, "inner");

        // A directory in the way that is not readable is just the next
        // parent's turn: the deep dir holds a .bi.toml that is itself a
        // directory, which no read_to_string can love.
        std::fs::create_dir(dir.join("outer/inner/deep/.bi.toml")).unwrap();
        let (path, _) = local_config_from(&dir.join("outer/inner/deep")).unwrap();
        assert_eq!(path, dir.join("outer/inner/.bi.toml"), "skipped in silence");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_local_config_anywhere_is_simply_none() {
        let dir = std::env::temp_dir().join(format!("bi-nolocal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The walk continues into /tmp and / above the test dir; none of
        // those should hold one either, but if some machine's does, finding
        // it is the correct answer rather than a failure — so the assertion
        // allows only "none, or something above the test dir".
        if let Some((path, _)) = local_config_from(&dir) {
            assert!(!path.starts_with(&dir), "{}", path.display());
        }
        std::fs::remove_dir_all(&dir).unwrap();
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
