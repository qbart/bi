//! `:!` shell commands — the pure core.
//!
//! Vim's `:!cmd` and its kin split into two shapes in bi, because bi has no
//! pty and its main loop must keep drawing while a command runs (see
//! `docs/specs/shell.md`, "What vim does, and what bi cannot"):
//!
//! - **Filters** (`:{range}!cmd`, `:r !cmd`, `:w !cmd`) are synchronous: the
//!   buffer edit *is* the result, so they run through the process runner
//!   formatters already use (`fmt::Run` / `ProcessRun`), under its 5 s guard.
//!   Nothing in this module is for them.
//! - **Bare `:!cmd`** is a **job**: it runs in the background, its output
//!   streams into a pane as it arrives, and the editor stays live. That is
//!   what [`Job`], [`Slot`] and [`Spawn`] exist for.
//!
//! [`ProcessSpawn`] is the real spawner: `sh -c <cmd>`, with **stdin closed
//! at spawn**. A program that prompts gets EOF instead of a hang — the
//! honest answer without a pty; interactive programs are what the terminal
//! is for. Two reader threads decode stdout and stderr as they arrive
//! (lossily — a job's output is not guaranteed UTF-8) and push [`Line`]s
//! into a [`Slot`]; a waiter thread notices the exit and files it there too.
//! Each push and the final exit calls `wake()` — the same shape as
//! `InstallSlot`/`pump_install` in `src/editor.rs`: a thread fills a
//! `Slot` and rings a waker, and the editor drains it in `settle`.
//!
//! The lib boundary holds here exactly as it does for LSP and DAP: the
//! [`Spawn`] trait is the seam, `ProcessSpawn` is the only thing in this
//! module that touches a process, and [`fake::FakeSpawn`] lets tests (and a
//! process-less embedding) drive the whole flow without one.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// One line of a job's output, tagged by which pipe it came from — arrival
/// order across both pipes is what a console wants, not stdout-then-stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Out(String),
    Err(String),
}

/// How a job ended. `Killed` is distinct from `Signal`: both mean "no exit
/// code", but `Killed` means bi sent the signal ([`Handle::kill`]), so the
/// editor can say "killed" instead of guessing at a death it caused itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    Signal,
    Killed,
}

/// What the reader and waiter threads fill and the editor drains. Lines
/// accumulate in arrival order; the exit is set once and taken once.
#[derive(Default)]
struct SlotInner {
    lines: Vec<Line>,
    exit: Option<Exit>,
}

/// A handle to one job's output, shared between the threads that produce it
/// and the editor thread that drains it in `settle` — the same shape as
/// `InstallSlot` in `src/editor.rs`.
#[derive(Clone, Default)]
pub struct Slot(Arc<Mutex<SlotInner>>);

impl Slot {
    /// Called from a reader thread: append one line.
    pub fn push(&self, line: Line) {
        self.0.lock().expect("shell slot poisoned").lines.push(line);
    }

    /// Called from the waiter thread (or `Handle::kill`): files the exit.
    /// Delivered once: `Killed` is sticky, so any call after it is dropped.
    /// `kill()` pins `Killed` before it forces the kill, so its verdict
    /// always outranks the waiter thread's for the same death — the editor
    /// asked for it, so its account of the death wins.
    pub fn finish(&self, exit: Exit) {
        let mut inner = self.0.lock().expect("shell slot poisoned");
        if inner.exit == Some(Exit::Killed) {
            return;
        }
        inner.exit = Some(exit);
    }

    /// Everything seen so far, and the exit if one has landed. Empties both
    /// — the next `drain` sees only what arrived since.
    pub fn drain(&self) -> (Vec<Line>, Option<Exit>) {
        let mut inner = self.0.lock().expect("shell slot poisoned");
        (std::mem::take(&mut inner.lines), inner.exit.take())
    }
}

/// A running (or just-finished) job, from the editor's side.
pub trait Handle: Send {
    /// Ends the process: SIGTERM, ~2 s grace, SIGKILL, reaped. Files
    /// [`Exit::Killed`] into the job's [`Slot`] once the process is
    /// confirmed gone, so the editor can tell "we killed it" from "it
    /// died" on its own.
    ///
    /// No `libc`/`nix` dependency exists in this crate and this method adds
    /// none: the SIGTERM is sent by shelling out to the `kill` binary
    /// (`kill -TERM <pid>`) rather than calling `libc::kill` directly. The
    /// final SIGKILL uses `std::process::Child::kill`, which needs no such
    /// dependency because the standard library already knows how to send
    /// that one signal.
    fn kill(&mut self);

    /// Whether the process is still alive, checked without blocking.
    fn is_running(&mut self) -> bool;
}

/// How a job comes to exist. The default spawns a process; tests and a
/// process-less embedding substitute their own — the editor never spawns
/// directly, the way it never calls `lsp::transport::Spawn` directly.
pub trait Spawn {
    #[allow(clippy::type_complexity, reason = "an alias would name it once and hide it")]
    fn spawn(
        &self,
        cmd: &str,
        cwd: &Path,
        slot: Slot,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Box<dyn Handle>, String>;
}

/// The real thing: `sh -c <cmd>` in `cwd`, stdin closed, stdout and stderr
/// piped and streamed into `slot` as they arrive.
pub struct ProcessSpawn;

impl Spawn for ProcessSpawn {
    fn spawn(
        &self,
        cmd: &str,
        cwd: &Path,
        slot: Slot,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Box<dyn Handle>, String> {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("sh: {e}"))?;

        let pid = child.id();
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        let stdout_reader = spawn_reader(stdout, slot.clone(), wake.clone(), Line::Out)?;
        let stderr_reader = spawn_reader(stderr, slot.clone(), wake.clone(), Line::Err)?;

        // The `Child` moves behind a mutex rather than into the waiter
        // thread outright: `Handle::kill` needs to reach it too (for the
        // final `child.kill()` + `child.wait()` once SIGTERM's grace period
        // runs out), and only one side can own it. The waiter polls
        // `try_wait` — never a blocking `wait` — so it holds the lock only
        // for an instant each pass, and `kill` is never shut out of it.
        let child = Arc::new(Mutex::new(child));

        let waiter_child = child.clone();
        let waiter_slot = slot.clone();
        let waiter_wake = wake.clone();
        thread::Builder::new()
            .name("shell-wait".into())
            .spawn(move || {
                loop {
                    let status = waiter_child.lock().expect("child mutex poisoned").try_wait();
                    match status {
                        Ok(Some(status)) => {
                            // Join the readers before filing the exit: a
                            // child can be reaped while its last lines still
                            // sit in the pipe buffer, unread. `join` gives
                            // the happens-before edge that `push`ing a line
                            // and storing the exit otherwise lack — every
                            // push the readers will ever make is visible
                            // here before `finish` runs, so "exit delivered"
                            // implies "all output delivered", and a drain
                            // that sees the exit never misses a line that
                            // preceded it.
                            //
                            // This arm is also reached on the kill race:
                            // `std::process::Child` caches its reaped
                            // status, so if `Handle::kill`'s forced path
                            // reaps the child first, our `try_wait` above
                            // does NOT error — it returns that same cached
                            // `Ok(Some(status))`, and we join the readers
                            // and compute an `exit` here same as any other
                            // exit. That's harmless, not because we skip
                            // this branch (we don't), but because the
                            // readers see EOF once the child is gone (the
                            // joins complete either way) and because
                            // `Slot::finish` is sticky on `Killed`: by the
                            // time we call it below, `kill()` has already
                            // pinned `Exit::Killed`, so our `finish(exit)`
                            // here is silently discarded. The stickiness in
                            // `finish`, not the `Err(_)` arm below, is what
                            // guards this race.
                            //
                            // Caveat, on both the natural-exit and kill
                            // paths alike: a grandchild that inherits and
                            // holds the stdout/stderr fds open past this
                            // child's own exit keeps the corresponding
                            // reader blocked on EOF, and now delays `finish`
                            // (and thus the editor's status line) along with
                            // it. Nothing new hangs that wasn't already
                            // hanging — that reader was already stuck
                            // waiting on the same fd before this join
                            // existed — and it never blocks `kill()` or the
                            // editor thread: `kill()` calls `finish(Killed)`
                            // directly, without ever joining this waiter,
                            // and the `Child` mutex is not held across
                            // either join.
                            let _ = stdout_reader.join();
                            let _ = stderr_reader.join();
                            let exit = match status.code() {
                                Some(code) => Exit::Code(code),
                                None => Exit::Signal,
                            };
                            waiter_slot.finish(exit);
                            waiter_wake();
                            return;
                        }
                        Ok(None) => thread::sleep(Duration::from_millis(15)),
                        // A genuine wait error — e.g. the pid vanished from
                        // under us — not the kill race: a `Child` that has
                        // already been reaped (by `kill()`'s forced path or
                        // otherwise) yields the cached `Ok(Some(status))`
                        // above on every subsequent `try_wait`, never `Err`.
                        Err(_) => return,
                    }
                }
            })
            .map_err(|e| format!("spawning wait thread: {e}"))?;

        Ok(Box::new(ProcessHandle { pid, child, slot, wake }))
    }
}

/// Reads `pipe` line by line (lossily — a job's output is not guaranteed
/// UTF-8) and pushes each as a `Line` into `slot`, waking after each.
/// `read_until` rather than `BufRead::lines`: `lines` errors out on invalid
/// UTF-8 and drops the rest of the stream, which a build's stray byte
/// should not be able to do.
///
/// Returns the thread's `JoinHandle` rather than discarding it: the waiter
/// thread joins both readers before filing the exit, so a `drain()` that
/// sees the exit can never be missing lines that were still sitting in the
/// pipe when the child was reaped.
fn spawn_reader(
    pipe: impl Read + Send + 'static,
    slot: Slot,
    wake: Arc<dyn Fn() + Send + Sync>,
    make: fn(String) -> Line,
) -> Result<thread::JoinHandle<()>, String> {
    thread::Builder::new()
        .name("shell-read".into())
        .spawn(move || {
            let mut reader = BufReader::new(pipe);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) => return,
                    Ok(_) => {
                        if buf.last() == Some(&b'\n') {
                            buf.pop();
                        }
                        slot.push(make(String::from_utf8_lossy(&buf).into_owned()));
                        wake();
                    }
                    Err(_) => return,
                }
            }
        })
        .map_err(|e| format!("spawning reader thread: {e}"))
}

/// The real [`Handle`]: a live `sh -c` child.
struct ProcessHandle {
    pid: u32,
    child: Arc<Mutex<Child>>,
    slot: Slot,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Handle for ProcessHandle {
    fn kill(&mut self) {
        if !self.is_running() {
            return;
        }

        // `spawn()`, not `status()`: fire-and-forget per the controller
        // ruling (no libc/nix dependency added), so this waits only for the
        // `kill` binary itself to exit (near-instant), not for the target
        // process — the `kill` process becomes a short-lived zombie until
        // reaped, by design.
        let _ = Command::new("kill").arg("-TERM").arg(self.pid.to_string()).spawn();

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && self.is_running() {
            thread::sleep(Duration::from_millis(10));
        }

        // Pin the verdict before forcing the kill: once this lands, `finish`
        // is sticky on `Killed`, so the waiter thread — which may be
        // blocked on the child mutex below and wakes the instant it's
        // dropped — can never overwrite it with `Signal`/`Code`, regardless
        // of scheduling. We asked for this death, so our verdict wins even
        // if the process happened to exit cleanly on the SIGTERM above.
        self.slot.finish(Exit::Killed);

        if self.is_running() {
            let mut child = self.child.lock().expect("child mutex poisoned");
            let _ = child.kill();
            let _ = child.wait();
        }

        (self.wake)();
    }

    fn is_running(&mut self) -> bool {
        let mut child = self.child.lock().expect("child mutex poisoned");
        !matches!(child.try_wait(), Ok(Some(_)))
    }
}

/// A running (or just-finished) shell job — pure state, no I/O of its own.
/// Filled in by `Spawn::spawn`; drained by the editor's `pump_shell`.
pub struct Job {
    pub cmd: String,
    pub slot: Slot,
    pub handle: Box<dyn Handle>,
    pub exit: Option<Exit>,
}

/// `%` expands to `current`, `#` to `alternate`; `\%` and `\#` are the
/// literal characters. `!` is untouched — bi drops vim's "expands to the
/// previous command" rule (see `docs/specs/shell.md`, "Resolved decisions").
pub fn expand(cmd: &str, current: Option<&str>, alternate: Option<&str>) -> Result<String, String> {
    let mut out = String::with_capacity(cmd.len());
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if matches!(chars.peek(), Some('%') | Some('#')) => {
                out.push(chars.next().expect("peeked Some above"));
            }
            '%' => out.push_str(current.ok_or("no file name for %")?),
            '#' => out.push_str(alternate.ok_or("no alternate file for #")?),
            other => out.push(other),
        }
    }
    Ok(out)
}

/// A spawner and handle that record instead of running a process, for
/// tests anywhere in the crate — the same shape as `lsp::transport::fake`
/// and `dap::transport::fake`.
#[cfg(test)]
pub mod fake {
    use super::*;

    #[derive(Default, Clone)]
    pub struct FakeSpawn {
        /// Every command spawned, with the slot it was given.
        pub spawned: Arc<Mutex<Vec<(String, Slot)>>>,
        /// How many `FakeHandle`s have been killed.
        pub killed: Arc<Mutex<usize>>,
    }

    impl Spawn for FakeSpawn {
        fn spawn(
            &self,
            cmd: &str,
            _cwd: &Path,
            slot: Slot,
            _wake: Arc<dyn Fn() + Send + Sync>,
        ) -> Result<Box<dyn Handle>, String> {
            self.spawned.lock().expect("spawned list poisoned").push((cmd.to_string(), slot));
            Ok(Box::new(FakeHandle { killed: self.killed.clone(), running: true }))
        }
    }

    pub struct FakeHandle {
        killed: Arc<Mutex<usize>>,
        running: bool,
    }

    impl Handle for FakeHandle {
        fn kill(&mut self) {
            *self.killed.lock().expect("killed count poisoned") += 1;
            self.running = false;
        }

        fn is_running(&mut self) -> bool {
            self.running
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn waker() -> (Arc<dyn Fn() + Send + Sync>, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = tx.send(());
        });
        (wake, rx)
    }

    /// Drains `slot` on every wake until an exit lands, accumulating lines
    /// across drains (a `drain` empties as it reads).
    fn run_to_exit(slot: &Slot, rx: &mpsc::Receiver<()>, patience: Duration) -> (Vec<Line>, Exit) {
        let mut lines = Vec::new();
        loop {
            rx.recv_timeout(patience).expect("the waker fired before the deadline");
            let (more, exit) = slot.drain();
            lines.extend(more);
            if let Some(exit) = exit {
                return (lines, exit);
            }
        }
    }

    #[test]
    fn percent_and_hash_expand_and_backslash_keeps_them_literal() {
        assert_eq!(expand("ls %", Some("a.rs"), None).unwrap(), "ls a.rs");
        assert_eq!(expand("diff % #", Some("a.rs"), Some("b.rs")).unwrap(), "diff a.rs b.rs");
        assert_eq!(expand(r"echo \% \#", None, None).unwrap(), "echo % #");
        assert!(expand("cat %", None, None).unwrap_err().contains("no file name"));
        assert_eq!(expand("grep '!' %", Some("x"), None).unwrap(), "grep '!' x", "! is literal");
    }

    #[test]
    fn a_slot_hands_back_lines_in_order_then_the_exit_once() {
        let s = Slot::default();
        s.push(Line::Out("a".into()));
        s.push(Line::Err("b".into()));
        s.finish(Exit::Code(0));
        assert_eq!(s.drain(), (vec![Line::Out("a".into()), Line::Err("b".into())], Some(Exit::Code(0))));
        assert_eq!(s.drain(), (vec![], None), "drained means drained");
    }

    // `run_to_exit` returns on the very first drain that shows an exit, so
    // its two `lines.contains` asserts below are already the check: an exit
    // is never seen before the output that preceded it (the waiter joins
    // both reader threads before filing the exit — see the join in
    // `ProcessSpawn::spawn`'s waiter thread — so this would fail on a
    // reader that hadn't yet been scheduled to push its line when the child
    // was reaped, which is exactly the race that fix round 2 closed).
    #[test]
    fn the_real_spawner_streams_stdout_and_stderr_and_reports_the_exit() {
        let slot = Slot::default();
        let (wake, rx) = waker();
        let _handle = ProcessSpawn
            .spawn("echo out; echo err 1>&2; exit 3", Path::new("."), slot.clone(), wake)
            .expect("sh exists everywhere this builds");

        let (lines, exit) = run_to_exit(&slot, &rx, Duration::from_secs(5));
        assert_eq!(exit, Exit::Code(3));
        assert!(lines.contains(&Line::Out("out".to_string())), "{lines:?}");
        assert!(lines.contains(&Line::Err("err".to_string())), "{lines:?}");
    }

    #[test]
    fn stdin_is_closed_so_a_prompting_command_ends() {
        let slot = Slot::default();
        let (wake, rx) = waker();
        let _handle = ProcessSpawn
            .spawn("read x; echo done", Path::new("."), slot.clone(), wake)
            .expect("sh exists everywhere this builds");

        let (_, exit) = run_to_exit(&slot, &rx, Duration::from_secs(5));
        assert!(matches!(exit, Exit::Code(0) | Exit::Code(1)), "{exit:?}");
    }

    #[test]
    fn kill_ends_a_sleeping_command() {
        let slot = Slot::default();
        let (wake, rx) = waker();
        let mut handle = ProcessSpawn
            .spawn("sleep 30", Path::new("."), slot.clone(), wake)
            .expect("sh exists everywhere this builds");

        let start = Instant::now();
        handle.kill();
        assert!(start.elapsed() < Duration::from_secs(3), "kill took {:?}", start.elapsed());

        let (_, exit) = run_to_exit(&slot, &rx, Duration::from_secs(3));
        assert_eq!(exit, Exit::Killed, "kill's verdict must win the race with the waiter thread");
        assert!(start.elapsed() < Duration::from_secs(3), "kill took {:?}", start.elapsed());
        assert!(!handle.is_running());
    }

    #[test]
    fn kill_still_reports_killed_when_the_job_honours_sigterm() {
        let slot = Slot::default();
        let (wake, rx) = waker();
        let mut handle = ProcessSpawn
            .spawn(
                "trap 'exit 0' TERM; while :; do sleep 0.1; done",
                Path::new("."),
                slot.clone(),
                wake,
            )
            .expect("sh exists everywhere this builds");

        let start = Instant::now();
        handle.kill();
        assert!(start.elapsed() < Duration::from_secs(3), "kill took {:?}", start.elapsed());

        let (_, exit) = run_to_exit(&slot, &rx, Duration::from_secs(3));
        assert_eq!(
            exit,
            Exit::Killed,
            "we asked for this death, so our verdict wins even though the job exited cleanly"
        );
        assert!(!handle.is_running());
    }
}
