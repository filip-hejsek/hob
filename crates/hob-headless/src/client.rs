//! The client side of the [protocol]: sends calls to the
//! headless frontend and passes on what it sends back.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::protocol::{self, Call, ErrorObject, Incoming, Notification};

/// A connection to the headless frontend.
///
/// Notifications are passed to a callback, in order, on a thread of the
/// client's. The callback has returned for all notifications a call causes
/// by the time [`call`](Client::call) returns.
pub struct Client {
    input: Box<dyn Write + Send>,
    replies: Receiver<(Value, Result<Value, ErrorObject>)>,
    next_id: u64,
    child: Option<Child>,
}

impl Client {
    /// Talks to a frontend through `input` (its stdin) and `output` (its
    /// stdout). Notifications are passed to `notify`.
    pub fn new(
        input: impl Write + Send + 'static,
        output: impl Read + Send + 'static,
        notify: impl FnMut(Notification) + Send + 'static,
    ) -> Client {
        let (replies_sender, replies) = mpsc::channel();
        thread::spawn(move || read(output, &replies_sender, notify));
        Client {
            input: Box::new(input),
            replies,
            next_id: 0,
            child: None,
        }
    }

    /// Starts the frontend with `command`, and talks to it over its stdin and
    /// stdout. Its stderr is inherited. Notifications are passed to `notify`.
    pub fn spawn(
        mut command: Command,
        notify: impl FnMut(Notification) + Send + 'static,
    ) -> io::Result<Client> {
        let mut child = (command.stdin(Stdio::piped()).stdout(Stdio::piped())).spawn()?;
        let input = child.stdin.take().expect("stdin is piped");
        let output = child.stdout.take().expect("stdout is piped");
        let mut client = Client::new(input, output, notify);
        client.child = Some(child);
        Ok(client)
    }

    /// The command for `hob --headless` from the directory of the running
    /// program; or, if `args` isn't empty, the command it gives.
    pub fn command(mut args: impl Iterator<Item = std::ffi::OsString>) -> io::Result<Command> {
        if let Some(program) = args.next() {
            let mut command = Command::new(program);
            command.args(args);
            return Ok(command);
        }
        let mut command = Command::new(std::env::current_exe()?.with_file_name("hob"));
        command.arg("--headless");
        Ok(command)
    }

    /// Calls a method, and waits for the reply. Fails if the frontend is gone.
    pub fn call(&mut self, call: &Call) -> io::Result<Result<Value, ErrorObject>> {
        self.call_timeout(call, None)
    }

    /// Like [`call`](Client::call), but waits for the reply at most for
    /// `timeout`; then fails with [`io::ErrorKind::TimedOut`]. A reply that
    /// comes later is ignored.
    pub fn call_timeout(
        &mut self,
        call: &Call,
        timeout: Option<Duration>,
    ) -> io::Result<Result<Value, ErrorObject>> {
        self.next_id += 1;
        writeln!(self.input, "{}", protocol::request(self.next_id, call))?;
        self.input.flush()?;
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            let reply = match deadline {
                Some(deadline) => self
                    .replies
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .map_err(|e| e == RecvTimeoutError::Timeout),
                None => self.replies.recv().map_err(|_| false),
            };
            match reply {
                Ok((id, result)) if id == self.next_id => return Ok(result),
                Ok(_) => {}
                Err(true) => {
                    let message = "the frontend didn't reply in time";
                    return Err(io::Error::new(io::ErrorKind::TimedOut, message));
                }
                Err(false) => {
                    let message = "the frontend exited";
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, message));
                }
            }
        }
    }
}

impl Drop for Client {
    /// Ends the frontend's input, which makes it exit, and waits for it if it
    /// was started by [`spawn`](Client::spawn).
    fn drop(&mut self) {
        self.input = Box::new(io::sink());
        if let Some(child) = &mut self.child {
            let _ = child.wait();
        }
    }
}

fn read(
    output: impl Read,
    replies: &Sender<(Value, Result<Value, ErrorObject>)>,
    mut notify: impl FnMut(Notification),
) {
    for line in BufReader::new(output).lines() {
        let Ok(line) = line else { return };
        match protocol::parse_incoming(&line) {
            Ok(Incoming::Response { id, result }) => {
                // The client may be gone; then the reply is ignored.
                let _ = replies.send((id, result));
            }
            Ok(Incoming::Notification(n)) => notify(n),
            Err(e) => eprintln!("hob: bad message from the headless frontend: {e}"),
        }
    }
}
