//! Hob's process: started with a command, and ended by ending its input, or
//! killed if it doesn't exit.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hob_headless::client::Client;
use hob_headless::protocol::{Call, ErrorObject, Notification};
use serde_json::Value;

/// How many of the last lines of hob's stderr are kept, to report when it
/// exits.
const STDERR_LINES: usize = 20;

/// The longest stderr line kept; the rest is cut off.
const STDERR_LINE_LENGTH: usize = 500;

/// How often to check whether hob has exited.
const POLL: Duration = Duration::from_millis(10);

/// After hob has exited, how long to wait for the rest of its output.
const DRAIN: Duration = Duration::from_secs(1);

/// What hob sends, as it arrives.
pub(crate) enum HobEvent {
    Notification(Notification),
    /// Its stdout has ended: it has exited, or will soon. Nothing comes after
    /// this.
    OutputEnded,
}

/// A running hob process.
pub(crate) struct Hob {
    pub(crate) pid: u32,
    /// `None` once its input is ended.
    client: Option<Client>,
    pub(crate) events: Receiver<HobEvent>,
    child: Child,
    stderr: Arc<Mutex<VecDeque<String>>>,
    stderr_thread: JoinHandle<()>,
    /// How long [`Drop`] waits for it to exit.
    grace: Duration,
    stopped: bool,
}

/// How hob ended.
pub(crate) struct Ended {
    /// Its exit status, unless waiting for it failed.
    pub(crate) status: io::Result<ExitStatus>,
    /// Whether it was killed, because it didn't exit by itself.
    pub(crate) killed: bool,
    /// The last lines of its stderr.
    pub(crate) stderr: Vec<String>,
}

impl Hob {
    /// Starts hob with `command`: the program and its arguments. Talks to it
    /// over its stdin and stdout; its stderr is passed on to ours. When
    /// dropped, it's stopped with `grace`.
    pub(crate) fn spawn(command: &[OsString], grace: Duration) -> io::Result<Hob> {
        let Some((program, args)) = command.split_first() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the command is empty",
            ));
        };
        let mut command = Command::new(program);
        command.args(args);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        // A process group of its own, so that killing it also kills the
        // processes it started (e.g. hob, when the command is `cargo run`).
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        let (sender, events) = mpsc::channel();
        // Dropped, and so sends `OutputEnded`, when the client stops reading.
        let guard = OutputGuard(sender);
        let notify = move |n| {
            let _ = guard.0.send(HobEvent::Notification(n));
        };
        let client = Client::new(ThreadWriter::new(stdin), stdout, notify);
        let lines = Arc::default();
        let stderr_thread = {
            let lines = Arc::clone(&lines);
            thread::Builder::new()
                .name("hob-mcp stderr".into())
                .spawn(move || read_stderr(stderr, &lines))?
        };
        Ok(Hob {
            pid: child.id(),
            client: Some(client),
            events,
            child,
            stderr: lines,
            stderr_thread,
            grace,
            stopped: false,
        })
    }

    /// Calls hob, and waits for the reply at most for `timeout`.
    pub(crate) fn call(
        &mut self,
        call: &Call,
        timeout: Duration,
    ) -> io::Result<Result<Value, ErrorObject>> {
        match &mut self.client {
            Some(client) => client.call_timeout(call, Some(timeout)),
            None => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }

    /// Ends hob's input, which makes it exit, and waits for it at most for
    /// `grace`; then kills it. Hob's events stay to be received.
    pub(crate) fn stop(&mut self, grace: Duration) -> Ended {
        self.stopped = true;
        self.client = None;
        let deadline = Instant::now() + grace;
        let mut killed = false;
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
                Ok(None) => {
                    killed = true;
                    kill(&mut self.child);
                    break self.child.wait();
                }
                Err(e) => break Err(e),
            }
        };
        // Its last words may still be on the way.
        let deadline = Instant::now() + DRAIN;
        while !self.stderr_thread.is_finished() && Instant::now() < deadline {
            thread::sleep(POLL);
        }
        let stderr = self.stderr.lock().unwrap().iter().cloned().collect();
        Ended {
            status,
            killed,
            stderr,
        }
    }

    /// Receives the events that are left after [`stop`](Hob::stop), until
    /// the output ends, or [`DRAIN`] passes.
    pub(crate) fn remaining_events(&self) -> impl Iterator<Item = HobEvent> {
        let deadline = Instant::now() + DRAIN;
        std::iter::from_fn(move || {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(HobEvent::OutputEnded) | Err(_) => None,
                Ok(event) => Some(event),
            }
        })
    }
}

impl Drop for Hob {
    fn drop(&mut self) {
        if !self.stopped {
            self.stop(self.grace);
        }
    }
}

/// Kills hob, with the processes it started.
fn kill(child: &mut Child) {
    #[cfg(unix)]
    {
        use rustix::process::{Pid, Signal, kill_process_group};
        // The process group's id is hob's PID. Hob isn't reaped yet, so the
        // id can't belong to anything else.
        if let Some(pid) = i32::try_from(child.id()).ok().and_then(Pid::from_raw)
            && kill_process_group(pid, Signal::KILL).is_ok()
        {
            return;
        }
    }
    let _ = child.kill();
}

/// Sends [`HobEvent::OutputEnded`] when dropped.
struct OutputGuard(Sender<HobEvent>);

impl Drop for OutputGuard {
    fn drop(&mut self) {
        let _ = self.0.send(HobEvent::OutputEnded);
    }
}

/// Writes on a thread of its own, so that a hob that doesn't read its input
/// can't block us.
struct ThreadWriter(Sender<Vec<u8>>);

impl ThreadWriter {
    fn new(mut stdin: ChildStdin) -> ThreadWriter {
        let (sender, receiver) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            for data in receiver {
                if stdin.write_all(&data).is_err() {
                    return;
                }
            }
        });
        ThreadWriter(sender)
    }
}

impl Write for ThreadWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match self.0.send(data.to_vec()) {
            Ok(()) => Ok(data.len()),
            Err(_) => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Passes hob's stderr on to ours, and keeps the last lines.
fn read_stderr(stderr: impl Read, lines: &Mutex<VecDeque<String>>) {
    let mut stderr = BufReader::new(stderr);
    let mut line = Vec::new();
    loop {
        line.clear();
        match stderr.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\n', '\r']);
        eprintln!("{text}");
        let mut text: String = text.into();
        if let Some((cut, _)) = text.char_indices().nth(STDERR_LINE_LENGTH) {
            text.truncate(cut);
            text += "…";
        }
        let mut lines = lines.lock().unwrap();
        if lines.len() == STDERR_LINES {
            lines.pop_front();
        }
        lines.push_back(text);
    }
}
