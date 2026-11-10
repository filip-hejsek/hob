//! The headless hob frontend: runs commands for a client program, which
//! talks to it over stdin and stdout with JSON-RPC (see [`protocol`]).
//!
//! Islands are handled by `kettle-ui-headless`; the client acts on them like
//! a user would (types text, presses buttons). Log records of the `log` crate
//! are sent to the client too. Nothing is saved.
//!
//! Like `hob-gui`, it doesn't know about specific commands: whoever calls
//! [`serve`] registers them.

pub mod client;
pub mod protocol;

use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::task::{Wake, Waker};
use std::thread;

use futures_executor::block_on_stream;
use hob_core::{Interpreter, LogRecord, Reply, Request, Requests};
use kettle_ui::Outcome;
use kettle_ui_headless::{ActionError, Island};
use serde_json::Value;

use protocol::{BlockId, Call, ErrorObject, IslandId, Notification, RunResult, code};

/// The default [`Options::log_prefix`], also used by the [`client`].
const LOG_PREFIX: &str = "hob";

/// How [`serve_with`] behaves.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Options {
    /// Whether islands sent to the client keep the paths of images read
    /// from files ([`Image::path`](kettle_ui::Image::path)), so a client on
    /// the same machine can use the file. Off by default: the client may be
    /// elsewhere, or the paths private. The image data is sent either way.
    pub image_paths: bool,
    /// Starts the server's messages on stderr, followed by `: `. Defaults to
    /// `hob`.
    pub log_prefix: String,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            image_paths: false,
            log_prefix: LOG_PREFIX.to_owned(),
        }
    }
}

/// Serves `interpreter` to a client, with the default [`Options`]: reads
/// calls from `input`, and writes replies and notifications to `output`.
/// Returns when `input` ends, or `output` is closed (a broken pipe): either
/// way, the client is gone.
///
/// Installs the `log` backend, if no other logger is installed yet.
pub fn serve(
    interpreter: Interpreter,
    input: impl Read + Send + 'static,
    output: impl Write,
) -> io::Result<()> {
    serve_with(interpreter, input, output, Options::default())
}

/// Like [`serve`], with `options`.
pub fn serve_with(
    interpreter: Interpreter,
    input: impl Read + Send + 'static,
    output: impl Write,
    options: Options,
) -> io::Result<()> {
    let (events, receiver) = mpsc::channel();
    let input_events = events.clone();
    thread::Builder::new()
        .name("hob-headless input".into())
        .spawn(move || read_input(input, input_events))?;
    let early_log = hob_core::logger::early_log_stderr(&options.log_prefix);
    let log_level = hob_core::logger::level_from_env(early_log);
    if let Some(records) = hob_core::logger::install(log_level, options.log_prefix.clone()) {
        let events = events.clone();
        thread::Builder::new()
            .name("hob-headless log".into())
            .spawn(move || forward(block_on_stream(records), events, Event::Log))?;
    }
    let mut server = Server {
        interpreter,
        options,
        output,
        events,
        next_id: 0,
        islands: HashMap::new(),
        records: Vec::new(),
        running: BTreeMap::new(),
    };
    // Dropping the live islands makes their commands' `show` fail.
    match server.serve(receiver) {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        result => result,
    }
}

/// Something the server reacts to.
enum Event {
    Input(io::Result<String>),
    InputEnd,
    /// A request from the command of a block.
    Request(BlockId, Request),
    Finished(BlockId),
    Log(LogRecord),
    /// Updates may have arrived for a live island.
    Wake(IslandId),
}

fn read_input(input: impl Read, events: Sender<Event>) {
    for line in BufReader::new(input).lines() {
        let failed = line.is_err();
        if events.send(Event::Input(line)).is_err() || failed {
            return;
        }
    }
    let _ = events.send(Event::InputEnd);
}

/// Sends the items as events until there are no more, or the server is gone.
fn forward<T>(items: impl Iterator<Item = T>, events: Sender<Event>, event: impl Fn(T) -> Event) {
    for item in items {
        if events.send(event(item)).is_err() {
            return;
        }
    }
}

struct Server<W> {
    interpreter: Interpreter,
    options: Options,
    output: W,
    /// For threads and wakers to send events.
    events: Sender<Event>,
    /// For block and island ids.
    next_id: u64,
    islands: HashMap<IslandId, Live>,
    /// All log records, for `Context::log_records`.
    records: Vec<LogRecord>,
    /// The command lines of running commands, for
    /// `Context::running_commands`.
    running: BTreeMap<BlockId, String>,
}

/// A live island.
struct Live {
    block: BlockId,
    island: Island,
    reply: Reply,
    /// Wakes the server when updates arrive.
    waker: Waker,
}

/// Sends [`Event::Wake`] for an island.
struct IslandWaker {
    island: IslandId,
    events: Sender<Event>,
}

impl Wake for IslandWaker {
    fn wake(self: Arc<Self>) {
        let _ = self.events.send(Event::Wake(self.island));
    }
}

type CallResult = Result<Value, ErrorObject>;

impl<W: Write> Server<W> {
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn send(&mut self, line: &str) -> io::Result<()> {
        writeln!(self.output, "{line}")?;
        self.output.flush()
    }

    fn notify(&mut self, mut notification: Notification) -> io::Result<()> {
        if let Notification::Island { contents, .. } = &mut notification
            && !self.options.image_paths
        {
            *contents = contents.map_images(kettle_ui::Image::without_path);
        }
        self.send(&protocol::notification(&notification))
    }

    /// Handles events until the input ends.
    fn serve(&mut self, events: Receiver<Event>) -> io::Result<()> {
        // The server keeps a sender, so this only ends by returning.
        for event in events {
            match event {
                Event::Input(line) => self.handle_line(&line?)?,
                Event::InputEnd => break,
                Event::Request(block, request) => self.handle_request(block, request)?,
                Event::Finished(block) => {
                    self.running.remove(&block);
                    self.notify(Notification::Finished { block })?;
                }
                Event::Log(record) => {
                    self.records.push(record.clone());
                    self.notify(Notification::Logged {
                        block: None,
                        record,
                    })?;
                }
                Event::Wake(island) => self.poll_island(island)?,
            }
        }
        Ok(())
    }

    fn handle_line(&mut self, line: &str) -> io::Result<()> {
        if line.trim().is_empty() {
            return Ok(());
        }
        let (id, result) = match protocol::parse_call(line) {
            Ok((id, call)) => (id, self.call(call)?),
            Err(bad) => (bad.id, Err(bad.error)),
        };
        match id {
            Some(id) => self.send(&protocol::response(&id, &result)),
            None => Ok(()),
        }
    }

    fn call(&mut self, call: Call) -> io::Result<CallResult> {
        match call {
            Call::Run { line } => Ok(self.run(&line)),
            Call::TypeText {
                island,
                field,
                text,
            } => self.act(island, |i| i.type_text(&field, &text).map(|()| None)),
            Call::SetChecked {
                island,
                field,
                checked,
            } => self.act(island, |i| i.set_checked(&field, checked).map(|()| None)),
            Call::Pick {
                island,
                field,
                line,
            } => self.act(island, |i| i.pick(&field, &line).map(|()| None)),
            Call::Press { island, label } => self.act(island, |i| i.press(&label).map(Some)),
        }
    }

    fn run(&mut self, line: &str) -> CallResult {
        let requests = (self.interpreter.run(line))
            .map_err(|e| ErrorObject::new(code::NOT_STARTED, e.to_string()))?;
        let block = self.next_id();
        self.pump(block, requests)
            .map_err(|e| ErrorObject::new(code::NOT_STARTED, e.to_string()))?;
        self.running
            .insert(block, hob_core::trim_line_end(line).to_owned());
        Ok(serde_json::to_value(RunResult { block }).expect("results serialize"))
    }

    /// Forwards a command's requests to the server, on a thread of its own.
    fn pump(&self, block: BlockId, requests: Requests) -> io::Result<()> {
        let events = self.events.clone();
        thread::Builder::new()
            .name(format!("hob-headless block {block}"))
            .spawn(move || {
                let finished = events.clone();
                forward(block_on_stream(requests), events, |r| {
                    Event::Request(block, r)
                });
                // The stream ended, so the command finished.
                let _ = finished.send(Event::Finished(block));
            })?;
        Ok(())
    }

    /// Does an action on a live island, then sends the island to the client.
    /// If the action has an outcome, the island is frozen and the outcome sent
    /// to its command.
    fn act(
        &mut self,
        id: IslandId,
        action: impl FnOnce(&mut Island) -> Result<Option<Outcome>, ActionError>,
    ) -> io::Result<CallResult> {
        let Some(live) = self.islands.get_mut(&id) else {
            let message = format!("no live island {id}");
            return Ok(Err(ErrorObject::new(code::NO_LIVE_ISLAND, message)));
        };
        let outcome = match action(&mut live.island) {
            Ok(outcome) => outcome,
            Err(e) => {
                // A failed submit makes the island show its errors.
                if let ActionError::Invalid(_) = e {
                    let (block, contents) = (live.block, live.island.freeze());
                    self.notify(Notification::Island {
                        block,
                        island: id,
                        live: true,
                        contents,
                    })?;
                }
                return Ok(Err(ErrorObject::new(code::ACTION_FAILED, e.to_string())));
            }
        };
        let (block, contents) = (live.block, live.island.freeze());
        let frozen = outcome.and_then(|outcome| Some((self.islands.remove(&id)?, outcome)));
        self.notify(Notification::Island {
            block,
            island: id,
            live: frozen.is_none(),
            contents,
        })?;
        if let Some((live, outcome)) = frozen {
            live.reply.send(outcome);
        }
        Ok(Ok(Value::Null))
    }

    /// Applies updates that arrived for an island, and sends it if it changed.
    fn poll_island(&mut self, id: IslandId) -> io::Result<()> {
        let Some(live) = self.islands.get_mut(&id) else {
            return Ok(());
        };
        let waker = live.waker.clone();
        if !live.island.poll_waking(&waker) {
            return Ok(());
        }
        let notification = Notification::Island {
            block: live.block,
            island: id,
            live: true,
            contents: live.island.freeze(),
        };
        self.notify(notification)
    }

    fn handle_request(&mut self, block: BlockId, request: Request) -> io::Result<()> {
        match request {
            Request::Print(text) => self.notify(Notification::Printed { block, text }),
            Request::Log(record) => {
                self.records.push(record.clone());
                let block = Some(block);
                self.notify(Notification::Logged { block, record })
            }
            Request::PrintIsland(contents) => {
                let island = self.next_id();
                self.notify(Notification::Island {
                    block,
                    island,
                    live: false,
                    contents,
                })
            }
            Request::Show(show) => {
                let (builder, reply) = show.into_parts();
                let mut island = match Island::new(builder) {
                    Ok(island) => island,
                    Err(e) => {
                        reply.fail(e);
                        return Ok(());
                    }
                };
                let id = self.next_id();
                let waker = Waker::from(Arc::new(IslandWaker {
                    island: id,
                    events: self.events.clone(),
                }));
                // Updates may have arrived already; this also sets the waker.
                island.poll_waking(&waker);
                let contents = island.freeze();
                self.islands.insert(
                    id,
                    Live {
                        block,
                        island,
                        reply,
                        waker,
                    },
                );
                self.notify(Notification::Island {
                    block,
                    island: id,
                    live: true,
                    contents,
                })
            }
            Request::LogRecords(reply) => {
                reply.send(self.records.clone());
                Ok(())
            }
            Request::RunningCommands(reply) => {
                let others = self.running.iter().filter(|&(&b, _)| b != block);
                reply.send(others.map(|(_, line)| line.clone()).collect());
                Ok(())
            }
            Request::Clear => self.notify(Notification::Cleared { block }),
            Request::Exit => self.notify(Notification::ExitRequested { block }),
        }
    }
}
