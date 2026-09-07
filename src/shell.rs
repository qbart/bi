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
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How long a SIGTERM has to work before the job is killed outright — the
/// same grace the debugger's `end_session` gives an adapter.
const GRACE: Duration = Duration::from_secs(2);

/// How long the waiter gives the two reader threads to reach EOF once the
/// job itself has been reaped. A grandchild that inherited stdout can hold
/// the pipe open for as long as it likes (`sh -c '(sleep 300 &)'`), and the
/// exit is a fact about the job, not about that fd: past this the waiter
/// files the exit and lets the readers finish whenever they finish. Their
/// pushes are still safe — the `Slot` mutex, not the join, is what makes
/// them visible — they just arrive after the trailer instead of before it.
const READER_GRACE: Duration = Duration::from_secs(1);

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
    /// Set by [`Slot::mark_killed`] the moment bi asks the job to die, and
    /// read by [`Slot::finish`]: whatever verdict the death then arrives
    /// with, it is reported as [`Exit::Killed`].
    killed: bool,
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

    /// Called from the escalation or waiter thread: files the exit. First
    /// one wins — whichever of them notices the death first is the one that
    /// reports it, and the loser's call is dropped.
    ///
    /// A job bi asked to kill is reported as [`Exit::Killed`] whatever it
    /// actually died of ([`Slot::mark_killed`]), including a clean `exit 0`
    /// out of its own SIGTERM handler: we asked for this death, so our
    /// account of it wins over the exit code it happened to wear.
    ///
    /// **The exit is the editor's signal that the job is over**, and
    /// `pump_shell` drops the `Job` — and with it `shutdown_shell`'s grip on
    /// the process — as soon as it sees one. So nothing files an exit until
    /// the process is confirmed gone: `kill` marks and signals, and leaves
    /// the filing to whichever thread watches the death happen.
    pub fn finish(&self, exit: Exit) {
        let mut inner = self.0.lock().expect("shell slot poisoned");
        if inner.exit.is_some() {
            return;
        }
        inner.exit = Some(if inner.killed { Exit::Killed } else { exit });
    }

    /// Called from `Handle::kill` before it signals anything: the death that
    /// follows is one bi asked for, so [`Slot::finish`] should report it as
    /// [`Exit::Killed`] regardless of what the process exits with.
    pub fn mark_killed(&self) {
        self.0.lock().expect("shell slot poisoned").killed = true;
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
    /// Ends the process, **without blocking the caller**: files
    /// [`Exit::Killed`] into the job's [`Slot`] (so the editor can tell "we
    /// killed it" from "it died" on its own), sends SIGTERM, and returns.
    /// The escalation — ~2 s of grace, then SIGKILL, then the reap — happens
    /// behind it; the waker rings when the process is actually gone.
    ///
    /// It has to be that way round: `:stop` runs on the editor thread, and a
    /// job that ignores SIGTERM would otherwise freeze the whole editor for
    /// the length of the grace period. [`Handle::kill_blocking`] is the
    /// variant for the one caller that needs the opposite.
    ///
    /// No `libc`/`nix` dependency exists in this crate and this method adds
    /// none: the signals are sent by shelling out to the `kill` binary
    /// rather than calling `libc::kill` directly, and the last-resort
    /// SIGKILL of the leader uses `std::process::Child::kill`, which the
    /// standard library already knows how to send.
    fn kill(&mut self);

    /// [`Handle::kill`] that returns only once the process is gone — for
    /// `Editor::shutdown_shell`, which runs as the frontend tears itself
    /// down: there is no event loop left to wake, so a detached escalation
    /// thread would be racing the exit of the process it is escalating in.
    ///
    /// The default is right for any handle whose `kill` is already
    /// synchronous (every fake, and any embedding that has no real process
    /// to outlive it); [`ProcessSpawn`]'s handle overrides it.
    fn kill_blocking(&mut self) {
        self.kill();
    }

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
            // Its own process group, with the shell as leader, so its pgid
            // is its pid. `sh -c` execs only a *simple* command: for
            // anything with a `;`, a `&` or a pipe in it the shell stays
            // alive as a parent, and signalling the leader alone would kill
            // the `sh` and orphan the `cargo build` underneath it. The group
            // is the unit a job actually is, so it is the unit `kill` aims
            // at (see `signal_group`). `process_group` is `std`'s own — it
            // adds no `libc`/`nix` dependency, which is the whole reason the
            // signals below go through the `kill` binary.
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("sh: {e}"))?;

        let pid = child.id();
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        let readers = Readers::default();
        spawn_reader(stdout, slot.clone(), wake.clone(), Line::Out, readers.clone())?;
        spawn_reader(stderr, slot.clone(), wake.clone(), Line::Err, readers.clone())?;

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
        let waiter_readers = readers.clone();
        thread::Builder::new()
            .name("shell-wait".into())
            .spawn(move || {
                loop {
                    let status = waiter_child.lock().expect("child mutex poisoned").try_wait();
                    match status {
                        Ok(Some(status)) => {
                            // Wait for the readers before filing the exit: a
                            // child can be reaped while its last lines still
                            // sit in the pipe buffer, unread, and "exit
                            // delivered" should imply "all output
                            // delivered" — a drain that sees the exit must
                            // not be missing a line that preceded it.
                            //
                            // Bounded, though (`READER_GRACE`): a grandchild
                            // holding the fd open would otherwise hold the
                            // trailer and the status line hostage for as
                            // long as it lives. Completeness is what the
                            // wait buys, not correctness — the `Slot` mutex
                            // is what makes a reader's pushes visible here,
                            // so the only cost of giving up is a late line
                            // landing under the trailer instead of above it.
                            //
                            // This arm is also reached on the kill race:
                            // `std::process::Child` caches its reaped
                            // status, so if the escalation thread reaps the
                            // child first, our `try_wait` above does NOT
                            // error — it returns that same cached
                            // `Ok(Some(status))`, and we compute an `exit`
                            // here same as any other exit. Which of the two
                            // threads files it does not matter: `finish`
                            // takes the first and drops the second, and a
                            // job that was killed is reported as `Killed`
                            // either way (`Slot::mark_killed`). That, not
                            // the `Err(_)` arm below, is what guards this
                            // race.
                            waiter_readers.wait_for_eof(READER_GRACE);
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
                        // already been reaped (by the escalation thread or
                        // otherwise) yields the cached `Ok(Some(status))`
                        // above on every subsequent `try_wait`, never `Err`.
                        //
                        // Returning silently would leave a phantom job: no
                        // exit is ever filed, so the editor keeps a `Job`
                        // that pumps nothing, refuses the next `:!`, and
                        // titles the pane `! <cmd>` forever. `Signal` is the
                        // honest verdict — the process is gone and we cannot
                        // say with what code — and it ends the job.
                        Err(_) => {
                            waiter_readers.wait_for_eof(READER_GRACE);
                            waiter_slot.finish(Exit::Signal);
                            waiter_wake();
                            return;
                        }
                    }
                }
            })
            .map_err(|e| format!("spawning wait thread: {e}"))?;

        Ok(Box::new(ProcessHandle { pid, child, slot, wake }))
    }
}

/// The two reader threads' finish line — see [`READER_GRACE`]: a count under
/// a mutex, notified on a condvar, because that is a wait a deadline can be
/// put on and a `JoinHandle::join` is not. Cloned into each reader, which
/// bumps the count as its very last act, and into the waiter.
#[derive(Clone, Default)]
struct Readers(Arc<(Mutex<usize>, Condvar)>);

impl Readers {
    /// Called by a reader thread as it ends, however it ends.
    fn done(&self) {
        let (count, ready) = &*self.0;
        *count.lock().expect("reader tally poisoned") += 1;
        ready.notify_all();
    }

    /// Blocks until both readers have hit EOF, or `patience` runs out.
    fn wait_for_eof(&self, patience: Duration) {
        let (count, ready) = &*self.0;
        let done = count.lock().expect("reader tally poisoned");
        let _ = ready.wait_timeout_while(done, patience, |count| *count < 2);
    }
}

/// Reads `pipe` line by line (lossily — a job's output is not guaranteed
/// UTF-8) and pushes each as a `Line` into `slot`, waking after each.
/// `read_until` rather than `BufRead::lines`: `lines` errors out on invalid
/// UTF-8 and drops the rest of the stream, which a build's stray byte
/// should not be able to do.
///
/// The thread is detached — it reports its own end through `readers`
/// instead, so the waiter can stop waiting on a reader that a grandchild is
/// keeping alive (see [`READER_GRACE`]).
fn spawn_reader(
    pipe: impl Read + Send + 'static,
    slot: Slot,
    wake: Arc<dyn Fn() + Send + Sync>,
    make: fn(String) -> Line,
    readers: Readers,
) -> Result<(), String> {
    thread::Builder::new()
        .name("shell-read".into())
        .spawn(move || {
            let mut reader = BufReader::new(pipe);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if buf.last() == Some(&b'\n') {
                            buf.pop();
                        }
                        slot.push(make(String::from_utf8_lossy(&buf).into_owned()));
                        wake();
                    }
                }
            }
            readers.done();
        })
        .map(|_| ())
        .map_err(|e| format!("spawning reader thread: {e}"))
}

/// `kill -<signal> -- -<pid>`: the leading `-` on the pid makes it a
/// *process group* id, and the job's group is its own (see the
/// `process_group(0)` in [`ProcessSpawn::spawn`]), so this reaches the whole
/// pipeline rather than only the `sh` at its head. `--` keeps `kill` from
/// reading `-1234` as an option.
///
/// `status()`, not `spawn()`: waiting for the `kill` binary itself is
/// near-instant and leaves no zombie behind, where fire-and-forget would
/// leave one unreaped for the life of the editor. Output is discarded — a
/// group that has already exited is not news, and the status line is not the
/// place for `kill`'s complaint about it.
fn signal_group(pid: u32, signal: &str) {
    let _ = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg("--")
        .arg(format!("-{pid}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Whether the child is still alive, reaping it if it is not.
fn running(child: &Mutex<Child>) -> bool {
    !matches!(child.lock().expect("child mutex poisoned").try_wait(), Ok(Some(_)))
}

/// SIGTERM has already been sent: give it [`GRACE`] to work, then take the
/// group out with SIGKILL and reap the leader — and only once the process is
/// actually gone, file the exit and wake the editor. Shared by the detached
/// thread [`Handle::kill`] leaves behind and by [`Handle::kill_blocking`];
/// the only difference between the two is which thread runs this.
///
/// The filing comes last on purpose. The exit is what tells `pump_shell` the
/// job is over, and it drops the `Job` when it sees one — so filing early
/// would hand the editor a "killed" for a process still in its grace period,
/// with nothing left holding the handle that `shutdown_shell` would need on
/// the way out. (The waiter thread may beat us to the filing; that is fine,
/// it only ever files a death it has already observed.)
fn escalate(pid: u32, child: &Mutex<Child>, slot: &Slot, wake: &(dyn Fn() + Send + Sync)) {
    let deadline = Instant::now() + GRACE;
    while Instant::now() < deadline && running(child) {
        thread::sleep(Duration::from_millis(10));
    }
    if running(child) {
        signal_group(pid, "KILL");
        // And the leader by hand as well: SIGKILL to the group is
        // best-effort (the `kill` binary may not be there, the group may
        // have changed under a job that called `setsid` itself), and
        // `Child::kill` is the one signal `std` sends without help. The
        // `wait` is the reap that stops it becoming a zombie.
        let mut child = child.lock().expect("child mutex poisoned");
        let _ = child.kill();
        let _ = child.wait();
    }
    slot.finish(Exit::Killed);
    wake();
}

/// The real [`Handle`]: a live `sh -c` child.
struct ProcessHandle {
    pid: u32,
    child: Arc<Mutex<Child>>,
    slot: Slot,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl ProcessHandle {
    /// The half `kill` and `kill_blocking` share: nothing to do if the
    /// process is already gone, otherwise pin the verdict and send SIGTERM.
    ///
    /// The mark comes *before* the signal, and that ordering is the whole
    /// point: whichever thread ends up filing the death that follows, it
    /// files it as `Killed`. What it deliberately does *not* do is file an
    /// exit here — see [`escalate`], which files one only once the process
    /// is confirmed gone.
    fn begin_kill(&mut self) -> bool {
        if !self.is_running() {
            return false;
        }
        self.slot.mark_killed();
        signal_group(self.pid, "TERM");
        true
    }
}

impl Handle for ProcessHandle {
    fn kill(&mut self) {
        if !self.begin_kill() {
            return;
        }
        // The escalation runs on its own thread so `:stop` costs the editor
        // thread one SIGTERM and nothing else. Detached on purpose: it owns
        // a clone of the `Arc<Mutex<Child>>`, so the child stays reapable
        // whatever happens to this handle. Nothing is filed or woken here —
        // the exit lands from `escalate`, once the process is really gone,
        // so the `Job` (and with it `shutdown_shell`'s grip on the process)
        // outlives the request to end it right up until it is honoured.
        let pid = self.pid;
        let child = self.child.clone();
        let slot = self.slot.clone();
        let wake = self.wake.clone();
        let _ = thread::Builder::new().name("shell-kill".into()).spawn(move || {
            escalate(pid, &child, &slot, &*wake);
        });
    }

    fn kill_blocking(&mut self) {
        if !self.begin_kill() {
            return;
        }
        escalate(self.pid, &self.child, &self.slot, &*self.wake);
    }

    fn is_running(&mut self) -> bool {
        running(&self.child)
    }
}

/// A running (or just-finished) shell job — pure state, no I/O of its own.
/// Filled in by `Spawn::spawn`; drained by the editor's `pump_shell`.
pub struct Job {
    pub cmd: String,
    pub slot: Slot,
    pub handle: Box<dyn Handle>,
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
            self.spawned.lock().expect("spawned list poisoned").push((cmd.to_string(), slot.clone()));
            Ok(Box::new(FakeHandle { killed: self.killed.clone(), running: true, slot }))
        }
    }

    pub struct FakeHandle {
        killed: Arc<Mutex<usize>>,
        running: bool,
        /// The same slot the editor holds. `kill` files [`Exit::Killed`]
        /// into it, matching [`Handle::kill`]'s documented contract — a
        /// caller that only checked `is_running` and never drained the slot
        /// would otherwise see a job that stays "running" forever.
        slot: Slot,
    }

    impl Handle for FakeHandle {
        fn kill(&mut self) {
            *self.killed.lock().expect("killed count poisoned") += 1;
            self.running = false;
            self.slot.finish(Exit::Killed);
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

        let (_, exit) = run_to_exit(&slot, &rx, Duration::from_secs(5));
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

        let (_, exit) = run_to_exit(&slot, &rx, Duration::from_secs(5));
        assert_eq!(
            exit,
            Exit::Killed,
            "we asked for this death, so our verdict wins even though the job exited cleanly"
        );
        assert!(!handle.is_running());
    }

    /// A job that ignores SIGTERM is what tells the two apart: with the
    /// escalation inline, `kill` would sit here for the whole 2 s grace
    /// period with the editor thread inside it.
    #[test]
    fn kill_returns_at_once_and_the_escalation_lands_behind_it() {
        let slot = Slot::default();
        let (wake, rx) = waker();
        let mut handle = ProcessSpawn
            .spawn("trap '' TERM; while :; do sleep 0.1; done", Path::new("."), slot.clone(), wake)
            .expect("sh exists everywhere this builds");

        let start = Instant::now();
        handle.kill();
        assert!(start.elapsed() < Duration::from_millis(50), "kill blocked for {:?}", start.elapsed());

        let (_, exit) = run_to_exit(&slot, &rx, Duration::from_secs(5));
        assert_eq!(exit, Exit::Killed);
        assert!(!handle.is_running());
    }

    /// What `shutdown_shell` needs on the way out: no detached thread racing
    /// the process's exit, because there is no editor left to wake.
    #[test]
    fn kill_blocking_returns_with_the_child_gone() {
        let slot = Slot::default();
        let (wake, _rx) = waker();
        let mut handle = ProcessSpawn
            .spawn("trap '' TERM; while :; do sleep 0.1; done", Path::new("."), slot.clone(), wake)
            .expect("sh exists everywhere this builds");

        handle.kill_blocking();

        assert!(!handle.is_running(), "kill_blocking must not return with the child alive");
        assert_eq!(slot.drain().1, Some(Exit::Killed));
    }

    /// `sh -c` only execs a *simple* command; `sleep 30 & wait` leaves the
    /// sleep a separate process in the job's group. Signalling the group,
    /// not the leader, is what stops it being orphaned — the marker file is
    /// the grandchild's proof of life.
    #[test]
    fn kill_reaches_the_whole_process_group() {
        let marker =
            std::env::temp_dir().join(format!("bi-shell-group-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let cmd = format!("(sleep 1; echo alive > {}) & wait", marker.display());

        let slot = Slot::default();
        let (wake, rx) = waker();
        let mut handle = ProcessSpawn
            .spawn(&cmd, Path::new("."), slot.clone(), wake)
            .expect("sh exists everywhere this builds");
        handle.kill();

        let (_, exit) = run_to_exit(&slot, &rx, Duration::from_secs(5));
        assert_eq!(exit, Exit::Killed);
        thread::sleep(Duration::from_millis(1600));
        assert!(!marker.exists(), "the grandchild outlived the group kill");
        let _ = std::fs::remove_file(&marker);
    }

    /// A grandchild that inherits stdout keeps the reader blocked long after
    /// the job itself is gone. The waiter gives the readers a bounded grace
    /// and then files the exit anyway: the editor learns the job ended when
    /// it ended, not when the last fd copy is closed.
    #[test]
    fn a_grandchild_holding_the_pipe_does_not_hold_up_the_exit() {
        let slot = Slot::default();
        let (wake, rx) = waker();
        let start = Instant::now();
        let _handle = ProcessSpawn
            .spawn("(sleep 5 &); echo started", Path::new("."), slot.clone(), wake)
            .expect("sh exists everywhere this builds");

        let (lines, exit) = run_to_exit(&slot, &rx, Duration::from_secs(3));
        assert_eq!(exit, Exit::Code(0));
        assert!(lines.contains(&Line::Out("started".to_string())), "{lines:?}");
        assert!(start.elapsed() < Duration::from_secs(4), "waited on the grandchild: {:?}", start.elapsed());
    }
}
