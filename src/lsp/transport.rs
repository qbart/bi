//! The child process and its threads — the only module in `lsp/` that spawns
//! anything.
//!
//! Three threads per server. A **reader** decodes frames off stdout and
//! delivers them to the [`Inbox`]; a **writer** owns stdin and drains a
//! channel, so the editor thread never blocks on a pipe behind a busy server;
//! a **stderr drain** keeps the last lines printed, because the first
//! question about a server that died is "what did it say".
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

use super::{Inbound, Inbox, ServerId, rpc};

/// One server's pipes, from the editor's side.
pub trait Transport: Send {
    /// Queues one message for the server. Never blocks on the pipe.
    fn send(&mut self, msg: &Value);
    /// The last lines the server printed to stderr, oldest first.
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
        server: ServerId,
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
        server: ServerId,
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
            .name(format!("lsp-read-{}", server.0))
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    match rpc::read_frame(&mut reader) {
                        Ok(Some(body)) => {
                            // A body that does not decode is one bad message,
                            // not a dead stream — skip it and keep reading.
                            if let Ok(msg) = rpc::decode(&body) {
                                inbox.deliver(server, msg);
                            }
                        }
                        Ok(None) | Err(_) => {
                            inbox.deliver(server, Inbound::Eof);
                            return;
                        }
                    }
                }
            })
            .map_err(|e| format!("spawning reader thread: {e}"))?;

        let (to_writer, frames) = mpsc::channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name(format!("lsp-write-{}", server.0))
            .spawn(move || {
                let mut stdin = stdin;
                // Ends when the channel closes (transport dropped or killed)
                // or the pipe breaks; either way stdin drops closed behind it,
                // which is a server's cue to exit.
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
            .name(format!("lsp-err-{}", server.0))
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

/// How much stderr is worth keeping. Enough for a stack trace; a bound, so a
/// server that logs forever costs a screenful and not the session's memory.
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
        // Reap, or the dead server sits as a zombie for the session's life.
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
/// anywhere in the crate. What the fake "server" answers is a test's to
/// script by delivering to the inbox it captured.
#[cfg(test)]
pub mod fake {
    use std::path::PathBuf;

    use super::*;

    #[derive(Default, Clone)]
    pub struct FakeSpawn {
        /// Every message any fake transport was asked to send.
        pub sent: Arc<Mutex<Vec<(ServerId, Value)>>>,
        /// The inbox each spawn captured, so a test can answer as the server.
        pub spawned: Arc<Mutex<Vec<(ServerId, Inbox, PathBuf)>>>,
        /// When set, every spawn fails with this — the missing binary.
        pub fail: Option<String>,
        pub killed: Arc<Mutex<Vec<ServerId>>>,
    }

    impl FakeSpawn {
        /// The methods sent so far by `server`, in order.
        pub fn methods(&self, server: ServerId) -> Vec<String> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .filter(|(id, _)| *id == server)
                .filter_map(|(_, m)| m.get("method").and_then(Value::as_str).map(String::from))
                .collect()
        }

        /// The last message sent by `server` with the given method.
        pub fn last(&self, server: ServerId, method: &str) -> Option<Value> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|(id, m)| *id == server && m["method"] == method)
                .map(|(_, m)| m.clone())
        }

        /// Answers the pending `initialize` (bi's first request, id 1) with
        /// the given capabilities, as the server would.
        pub fn grant(&self, server: ServerId, capabilities: Value) {
            let inbox = self
                .spawned
                .lock()
                .unwrap()
                .iter()
                .find(|(id, ..)| *id == server)
                .map(|(_, inbox, _)| inbox.clone())
                .expect("no such spawn");
            inbox.deliver(
                server,
                Inbound::Response {
                    id: 1,
                    result: Ok(serde_json::json!({ "capabilities": capabilities })),
                },
            );
        }
    }

    impl Spawn for FakeSpawn {
        fn spawn(
            &self,
            server: ServerId,
            _command: &[String],
            root: &Path,
            inbox: Inbox,
        ) -> Result<Box<dyn Transport>, String> {
            if let Some(reason) = &self.fail {
                return Err(reason.clone());
            }
            self.spawned.lock().unwrap().push((server, inbox, root.to_path_buf()));
            Ok(Box::new(FakeTransport {
                server,
                sent: self.sent.clone(),
                killed: self.killed.clone(),
            }))
        }
    }

    struct FakeTransport {
        server: ServerId,
        sent: Arc<Mutex<Vec<(ServerId, Value)>>>,
        killed: Arc<Mutex<Vec<ServerId>>>,
    }

    impl Transport for FakeTransport {
        fn send(&mut self, msg: &Value) {
            self.sent.lock().unwrap().push((self.server, msg.clone()));
        }

        fn stderr_tail(&self) -> Vec<String> {
            Vec::new()
        }

        fn exit_status(&mut self) -> Option<i32> {
            None
        }

        fn kill(&mut self) {
            self.killed.lock().unwrap().push(self.server);
        }

        fn wait_or_kill(&mut self, _patience: Duration) {
            self.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one test that touches a real process: `cat` echoes bi's own frame
    /// back through the reader thread, proving spawn, both pipes, framing and
    /// the waker with no external dependency.
    #[test]
    fn cat_echoes_a_frame_back_through_the_reader_thread() {
        let inbox = Inbox::default();
        let (tx, rx) = mpsc::channel();
        inbox.set_waker(move || {
            let _ = tx.send(());
        });

        let id = ServerId(0);
        let mut transport = ProcessSpawn
            .spawn(id, &["cat".to_string()], Path::new("/"), inbox.clone())
            .expect("cat exists everywhere this builds");

        transport.send(&rpc::request(1, "echo", serde_json::json!({ "x": 1 })));

        rx.recv_timeout(Duration::from_secs(5)).expect("the waker fired");
        let delivered = inbox.drain();
        match &delivered[..] {
            [(from, Inbound::Request { method, .. })] => {
                assert_eq!(*from, id);
                assert_eq!(method, "echo");
            }
            other => panic!("{other:?}"),
        }

        transport.kill();
    }

    #[test]
    fn a_missing_binary_is_an_error_naming_it() {
        let err = ProcessSpawn
            .spawn(
                ServerId(0),
                &["bi-test-no-such-binary".to_string()],
                Path::new("/"),
                Inbox::default(),
            )
            .err()
            .expect("cannot exist");
        assert!(err.contains("bi-test-no-such-binary"), "{err}");
    }

    #[test]
    fn a_dying_process_delivers_eof() {
        let inbox = Inbox::default();
        let (tx, rx) = mpsc::channel();
        inbox.set_waker(move || {
            let _ = tx.send(());
        });

        let mut transport = ProcessSpawn
            .spawn(ServerId(3), &["true".to_string()], Path::new("/"), inbox.clone())
            .unwrap();

        rx.recv_timeout(Duration::from_secs(5)).expect("eof wakes");
        assert!(matches!(inbox.drain()[..], [(ServerId(3), Inbound::Eof)]));
        // And the exit status is readable afterwards.
        transport.wait_or_kill(Duration::from_secs(1));
        assert_eq!(transport.exit_status(), Some(0));
    }
}
