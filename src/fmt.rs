//! External formatters — the `:fmt` filter run. See `docs/specs/fmt.md`.
//!
//! A tool is a stdin→stdout filter: the whole buffer in, the whole formatted
//! file out, exit 0. The core never spawns processes itself — the frontend
//! installs a [`Run`], the same arrangement `lsp::transport::Spawn` makes.

use std::path::Path;

/// One tool's definition, from `[fmt.tools.<name>]` — built-in defaults
/// with the user's config merged over them field-wise, like the server
/// table's. See `docs/specs/fmt.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolConfig {
    pub enabled: bool,
    /// argv. Empty means "defined but unusable", which the lookup skips.
    pub command: Vec<String>,
    /// The filetype names `crate::syntax::filetype` produces.
    pub filetypes: Vec<String>,
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self { enabled: true, command: Vec::new(), filetypes: Vec::new() }
    }
}

/// How a filter actually runs — supplied by the frontend, faked by tests.
///
/// The contract: feed `input` to `argv` started in `cwd`, hand back its
/// stdout on exit 0, and its first line of stderr (or the spawn error) as
/// the `Err` otherwise. Implementations own the guard against a tool that
/// hangs; the editor blocks on this call on purpose — see the spec for why
/// the synchronous run is the right trade.
pub trait Run {
    fn run(&self, argv: &[String], cwd: &Path, input: &str) -> Result<String, String>;
}

/// The real thing: a child process, fed and drained over pipes, with a guard
/// that kills a tool that hangs — the run blocks the editor on purpose (see
/// the spec), and the guard is what turns a broken formatter into a status
/// line instead of a hung session.
pub struct ProcessRun {
    pub guard: std::time::Duration,
}

impl Default for ProcessRun {
    fn default() -> Self {
        Self { guard: std::time::Duration::from_secs(5) }
    }
}

impl Run for ProcessRun {
    fn run(&self, argv: &[String], cwd: &Path, input: &str) -> Result<String, String> {
        use std::io::{Read, Write};
        use std::process::{Command, Stdio};

        let (name, args) = argv.split_first().ok_or("empty command")?;
        let mut child = Command::new(name)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{name}: {e}"))?;

        // Feed stdin from its own thread: a tool that writes faster than it
        // reads would otherwise deadlock against a full pipe. Dropping the
        // handle is the EOF the filter is waiting for.
        let mut stdin = child.stdin.take().expect("piped above");
        let text = input.to_string();
        let feeder = std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });

        // And drain both pipes from theirs, for the mirror image of the same
        // reason: a pipe holds about 64 KB, so a tool whose output is bigger
        // than that blocks in `write` until somebody reads it — and the
        // guard below only polls `try_wait`, which reads nothing. The tool
        // would then never exit and the guard would kill it as "hung",
        // which is exactly what `:%!sort` on a two-thousand-line file did.
        // Reading must overlap the wait, not follow it (the shape
        // `shell.rs`'s reader threads use), and it is `read_to_end` rather
        // than a line loop because a filter's answer is one blob of text,
        // not a stream anyone watches arrive.
        let mut stdout = child.stdout.take().expect("piped above");
        let mut stderr = child.stderr.take().expect("piped above");
        let out_drain = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stdout.read_to_end(&mut buf);
            buf
        });
        let err_drain = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf);
            buf
        });

        // The guard: poll rather than block, kill on expiry.
        let deadline = std::time::Instant::now() + self.guard;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = feeder.join();
                    // The two drains are left to end on their own: they are
                    // blocked on a read that only EOF ends, and a grandchild
                    // still holding the pipe would make joining them the
                    // very hang the guard just broke. Their output is not
                    // wanted anyway — this run has no answer.
                    return Err(format!("{name} hung — killed after {:?}", self.guard));
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
                Err(e) => return Err(format!("{name}: {e}")),
            }
        };
        // The child has exited, so both pipes are at EOF and these joins are
        // the last few bytes, not a wait — the same join `wait_with_output`
        // used to do inside itself.
        let stdout = out_drain.join().unwrap_or_default();
        let stderr = err_drain.join().unwrap_or_default();
        let _ = feeder.join();
        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr);
            let first = stderr.lines().find(|l| !l.trim().is_empty());
            return Err(first.map(str::to_string).unwrap_or_else(|| format!("{name}: {status}")));
        }
        String::from_utf8(stdout).map_err(|_| format!("{name}: output is not UTF-8"))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn run(argv: &[&str]) -> Result<String, String> {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        ProcessRun::default().run(&argv, Path::new("/"), "fn a\nfn b\n")
    }

    #[test]
    fn the_filter_gets_stdin_and_its_stdout_comes_back() {
        assert_eq!(run(&["tr", "a-z", "A-Z"]), Ok("FN A\nFN B\n".to_string()));
    }

    #[test]
    fn a_nonzero_exit_is_the_first_line_of_stderr() {
        let err = run(&["sh", "-c", "echo 'oops: bad input' >&2; echo partial; exit 1"]);
        assert_eq!(err, Err("oops: bad input".to_string()));
    }

    #[test]
    fn a_nonzero_exit_with_a_mute_stderr_still_says_something() {
        let err = run(&["sh", "-c", "exit 3"]).unwrap_err();
        assert!(err.contains("exit"), "{err}");
        assert!(err.contains('3'), "{err}");
    }

    #[test]
    fn a_missing_binary_is_an_error_not_a_panic() {
        assert!(run(&["bi-no-such-formatter"]).is_err());
    }

    /// A pipe holds about 64 KB. Polling `try_wait` without reading it
    /// deadlocks any tool that writes more: it blocks in `write`, never
    /// exits, and the guard kills it as "hung" — which is what `:%!sort` on
    /// a two-thousand-line file used to do. 150 KB through a short guard is
    /// that regression, and the guard being short is the proof there was no
    /// waiting involved.
    #[test]
    fn a_tool_that_outruns_the_pipe_buffer_is_drained_not_killed() {
        let argv: Vec<String> =
            ["sh", "-c", "yes | head -c 150000"].iter().map(|s| s.to_string()).collect();
        let runner = ProcessRun { guard: Duration::from_secs(2) };

        let out = runner.run(&argv, Path::new("/"), "").expect("drained, not killed");

        assert_eq!(out.len(), 150_000);
    }

    #[test]
    fn a_tool_that_hangs_is_killed_at_the_guard() {
        let argv: Vec<String> = ["sh", "-c", "sleep 10"].iter().map(|s| s.to_string()).collect();
        let runner = ProcessRun { guard: Duration::from_millis(50) };
        let err = runner.run(&argv, Path::new("/"), "").unwrap_err();
        assert!(err.contains("hung"), "{err}");
    }
}

/// A runner that answers from a script and records what it was asked —
/// the tests' seat, like `lsp::transport::fake`.
pub mod fake {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    pub struct FakeRun {
        /// Every call, as `(argv, cwd, stdin)`.
        pub calls: Arc<Mutex<Vec<(Vec<String>, PathBuf, String)>>>,
        answer: Arc<Mutex<Result<String, String>>>,
    }

    impl FakeRun {
        pub fn answering(answer: Result<String, String>) -> Self {
            Self { calls: Default::default(), answer: Arc::new(Mutex::new(answer)) }
        }
    }

    impl super::Run for FakeRun {
        fn run(&self, argv: &[String], cwd: &Path, input: &str) -> Result<String, String> {
            self.calls.lock().unwrap().push((argv.to_vec(), cwd.into(), input.to_string()));
            self.answer.lock().unwrap().clone()
        }
    }
}
