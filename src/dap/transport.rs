//! The adapter child process and its threads — the only module in `dap/`
//! that spawns anything.
//!
//! Three threads per session, mirroring `lsp::transport` exactly: a
//! **reader** decodes frames off stdout and delivers them to the [`Inbox`];
//! a **writer** owns stdin and drains a channel, so the editor thread never
//! blocks on a pipe behind a busy adapter; a **stderr drain** keeps the last
//! lines printed, because the first question about an adapter that died is
//! "what did it say".
//!
//! Both traits exist for the seam, not for ceremony: a test hands the
//! registry a transport that records instead of writing, and an embedding
//! host with no processes — a WASM sandbox — supplies its own [`Spawn`].

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{Inbound, Inbox, SessionId, rpc};

/// One session's pipes, from the editor's side.
pub trait Transport: Send {
    /// Queues one message for the adapter. Never blocks on the pipe.
    fn send(&mut self, msg: &Value);
    /// The last lines the adapter printed to stderr, oldest first.
    fn stderr_tail(&self) -> Vec<String>;
    /// The exit code, if the process has exited. `-1` stands for a signal.
    fn exit_status(&mut self) -> Option<i32>;
    /// Ends the process now.
    fn kill(&mut self);
    /// Waits up to `patience` for a voluntary exit, then kills.
    fn wait_or_kill(&mut self, patience: Duration);
}

/// How a transport comes to exist. The default spawns a process; tests and
/// process-less embeddings substitute their own.
pub trait Spawn {
    fn spawn(
        &self,
        session: SessionId,
        command: &[String],
        root: &Path,
        inbox: Inbox,
    ) -> Result<Box<dyn Transport>, String>;
}

/// The real thing: `command[0]` run in `root`, stdio piped.
pub struct ProcessSpawn;

impl Spawn for ProcessSpawn {
    fn spawn(
        &self,
        session: SessionId,
        command: &[String],
        root: &Path,
        inbox: Inbox,
    ) -> Result<Box<dyn Transport>, String> {
        let program = command.first().ok_or("empty command")?;
        let mut child = Command::new(program)
            .args(&command[1..])
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{program}: {e}"))?;

        let stdout = child.stdout.take().expect("stdout was piped");
        let stdin = child.stdin.take().expect("stdin was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        std::thread::Builder::new()
            .name(format!("dap-read-{}", session.0))
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    match rpc::read_frame(&mut reader) {
                        Ok(Some(body)) => {
                            // A body that does not decode is one bad message,
                            // not a dead stream — skip it and keep reading.
                            if let Ok(msg) = rpc::decode(&body) {
                                inbox.deliver(session, msg);
                            }
                        }
                        Ok(None) | Err(_) => {
                            inbox.deliver(session, Inbound::Eof);
                            return;
                        }
                    }
                }
            })
            .map_err(|e| format!("spawning reader thread: {e}"))?;

        let (to_writer, frames) = mpsc::channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name(format!("dap-write-{}", session.0))
            .spawn(move || {
                let mut stdin = stdin;
                // Ends when the channel closes (transport dropped or killed)
                // or the pipe breaks; either way stdin drops closed behind
                // it, which is an adapter's cue to exit.
                for frame in frames {
                    if stdin.write_all(&frame).and_then(|_| stdin.flush()).is_err() {
                        return;
                    }
                }
            })
            .map_err(|e| format!("spawning writer thread: {e}"))?;

        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let ring = tail.clone();
        std::thread::Builder::new()
            .name(format!("dap-err-{}", session.0))
            .spawn(move || {
                for line in BufReader::new(stderr).lines() {
                    let Ok(line) = line else { return };
                    let mut ring = ring.lock().expect("stderr ring poisoned");
                    if ring.len() >= STDERR_KEPT {
                        ring.pop_front();
                    }
                    ring.push_back(line);
                }
            })
            .map_err(|e| format!("spawning stderr thread: {e}"))?;

        Ok(Box::new(Process { child, to_writer: Some(to_writer), tail }))
    }
}

/// How much stderr is worth keeping. Enough for a stack trace; a bound, so
/// an adapter that logs forever costs a screenful and not the session's
/// memory.
const STDERR_KEPT: usize = 50;

struct Process {
    child: Child,
    /// `None` once closed — dropping the sender ends the writer thread, and
    /// stdin closes behind it.
    to_writer: Option<mpsc::Sender<Vec<u8>>>,
    tail: Arc<Mutex<VecDeque<String>>>,
}

impl Transport for Process {
    fn send(&mut self, msg: &Value) {
        if let Some(tx) = &self.to_writer {
            // A closed channel means the writer already hit a broken pipe;
            // the reader's Eof is on its way and will say so.
            let _ = tx.send(rpc::encode(msg));
        }
    }

    fn stderr_tail(&self) -> Vec<String> {
        self.tail.lock().expect("stderr ring poisoned").iter().cloned().collect()
    }

    fn exit_status(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.code().unwrap_or(-1)),
            _ => None,
        }
    }

    fn kill(&mut self) {
        self.to_writer = None;
        let _ = self.child.kill();
        // Reap, or the dead adapter sits as a zombie for the session's life.
        let _ = self.child.wait();
    }

    fn wait_or_kill(&mut self, patience: Duration) {
        self.to_writer = None;
        let deadline = Instant::now() + patience;
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.kill();
    }
}

/// A transport and spawner that record instead of doing I/O, for tests
/// anywhere in the crate. What the fake "adapter" answers is a test's to
/// script by delivering to the inbox it captured — see `event` and
/// `respond` below, which later tasks' tests use to drive a client through
/// a whole session without a real process.
#[cfg(test)]
pub mod fake {
    use std::path::PathBuf;

    use super::*;

    #[derive(Default, Clone)]
    pub struct FakeSpawn {
        /// Every message any fake transport was asked to send.
        pub sent: Arc<Mutex<Vec<(SessionId, Value)>>>,
        /// The inbox each spawn captured, so a test can answer as the adapter.
        pub spawned: Arc<Mutex<Vec<(SessionId, Inbox, PathBuf)>>>,
        /// When set, every spawn fails with this — the missing binary.
        pub fail: Option<String>,
        pub killed: Arc<Mutex<Vec<SessionId>>>,
    }

    impl FakeSpawn {
        /// The commands sent so far by `session`, in order.
        pub fn methods(&self, session: SessionId) -> Vec<String> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .filter(|(id, _)| *id == session)
                .filter_map(|(_, m)| m.get("command").and_then(Value::as_str).map(String::from))
                .collect()
        }

        /// The last message sent by `session` with the given command.
        pub fn last(&self, session: SessionId, command: &str) -> Option<Value> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|(id, m)| *id == session && m["command"] == command)
                .map(|(_, m)| m.clone())
        }

        /// The inbox captured for `session`'s spawn, so a helper can deliver
        /// to it as the adapter would.
        fn inbox(&self, session: SessionId) -> Inbox {
            self.spawned
                .lock()
                .unwrap()
                .iter()
                .find(|(id, ..)| *id == session)
                .map(|(_, inbox, _)| inbox.clone())
                .expect("no such spawn")
        }

        /// Delivers a spontaneous `event` from the adapter, as `stopped` or
        /// `terminated` would arrive unprompted.
        pub fn event(&self, session: SessionId, event: &str, body: Value) {
            self.inbox(session).deliver(session, Inbound::Event { event: event.to_string(), body });
        }

        /// Answers a pending request with a `response`, as the adapter
        /// would — success or failure, matched back by `request_seq`.
        pub fn respond(
            &self,
            session: SessionId,
            request_seq: i64,
            command: &str,
            success: bool,
            body: Value,
        ) {
            self.inbox(session).deliver(
                session,
                Inbound::Response {
                    request_seq,
                    success,
                    command: command.to_string(),
                    body,
                    message: None,
                },
            );
        }
    }

    impl Spawn for FakeSpawn {
        fn spawn(
            &self,
            session: SessionId,
            _command: &[String],
            root: &Path,
            inbox: Inbox,
        ) -> Result<Box<dyn Transport>, String> {
            if let Some(reason) = &self.fail {
                return Err(reason.clone());
            }
            self.spawned.lock().unwrap().push((session, inbox, root.to_path_buf()));
            Ok(Box::new(FakeTransport {
                session,
                sent: self.sent.clone(),
                killed: self.killed.clone(),
            }))
        }
    }

    struct FakeTransport {
        session: SessionId,
        sent: Arc<Mutex<Vec<(SessionId, Value)>>>,
        killed: Arc<Mutex<Vec<SessionId>>>,
    }

    impl Transport for FakeTransport {
        fn send(&mut self, msg: &Value) {
            self.sent.lock().unwrap().push((self.session, msg.clone()));
        }

        fn stderr_tail(&self) -> Vec<String> {
            Vec::new()
        }

        fn exit_status(&mut self) -> Option<i32> {
            None
        }

        fn kill(&mut self) {
            self.killed.lock().unwrap().push(self.session);
        }

        fn wait_or_kill(&mut self, _patience: Duration) {
            self.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// The one test that touches a real process: `cat` echoes bi's own frame
    /// back through the reader thread, proving spawn, both pipes, framing
    /// and the waker with no external dependency. `cat` echoes any bytes
    /// unchanged, so a request bi sends comes back as a reverse request —
    /// same `type: "request"` shape, other direction.
    #[test]
    fn cat_echoes_a_frame_back_through_the_reader_thread() {
        let inbox = Inbox::default();
        let (tx, rx) = mpsc::channel();
        inbox.set_waker(move || {
            let _ = tx.send(());
        });
        let id = SessionId(0);
        let mut t = ProcessSpawn
            .spawn(id, &["cat".to_string()], std::path::Path::new("/"), inbox.clone())
            .expect("cat exists");
        // cat echoes a well-formed frame; a request comes back as a reverse request.
        t.send(&crate::dap::rpc::request(1, "initialize", serde_json::json!({})));
        rx.recv_timeout(std::time::Duration::from_secs(5)).expect("waker fired");
        match &inbox.drain()[..] {
            [(from, Inbound::ReverseRequest { command, .. })] => {
                assert_eq!(*from, id);
                assert_eq!(command, "initialize");
            }
            other => panic!("{other:?}"),
        }
        t.kill();
    }

    #[test]
    fn a_missing_binary_is_an_error_naming_it() {
        let err = ProcessSpawn
            .spawn(
                SessionId(0),
                &["bi-no-such-adapter".into()],
                std::path::Path::new("/"),
                Inbox::default(),
            )
            .err()
            .expect("cannot exist");
        assert!(err.contains("bi-no-such-adapter"), "{err}");
    }
}
