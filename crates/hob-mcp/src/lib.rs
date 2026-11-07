//! An MCP server that lets an agent use hob: a bridge to the headless
//! frontend (`hob --headless`).
//!
//! The agent runs commands and acts on their islands with tools (see
//! [`mcp`]). Hob doesn't wait for the agent: commands print, islands change
//! and commands finish in the meantime. All of that is collected, and every
//! tool result starts with what happened since the last one, so the agent
//! never misses anything. The `wait` tool waits for more. A result shows
//! new islands in full, but changed ones only if they're of the command it's
//! about, so a progress bar elsewhere doesn't fill every result.
//!
//! Hob runs as a child process, which the bridge starts with a command: a
//! fixed one, or one the agent gives (see [`Launch`]). If restarting is
//! allowed, hob ends when a command asks it to or the agent closes it, and
//! after it ends (or crashes) the agent can start it again.

pub mod mcp;
mod process;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt::Write;
use std::io;
use std::time::{Duration, Instant};

use hob_core::Level;
use hob_headless::protocol::{BlockId, Call, IslandId, Notification};
use kettle_ui::FrozenIsland;
use serde_json::Value;

use process::{Hob, HobEvent};

/// After something happens, how long to wait for more before reporting, so
/// that things that happen together are reported together.
const SETTLE: Duration = Duration::from_millis(100);

/// The longest a report waits for things to settle.
const MAX_SETTLE: Duration = Duration::from_millis(500);

/// How hob is started.
#[derive(Clone, Debug)]
pub enum Launch {
    /// With this command (the program and its arguments); the agent can't
    /// give another.
    Command {
        /// The program and its arguments.
        command: Vec<OsString>,
        /// Whether the agent can start hob again, and end it. Otherwise, a
        /// command's request to exit does nothing, and once hob has exited,
        /// it's over (see [`Bridge::is_over`]).
        restart: bool,
    },
    /// With the command the agent gives, as often as it likes.
    ByAgent,
}

/// How long the bridge waits for hob.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// For a reply to a call; after that, hob may be stuck.
    pub call: Duration,
    /// For hob to exit after its input ends; after that, it's killed.
    pub grace: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            call: Duration::from_secs(10),
            grace: Duration::from_secs(3),
        }
    }
}

/// Hob, as the agent sees it.
pub struct Bridge {
    launch: Launch,
    limits: Limits,
    /// `None` while hob isn't running.
    hob: Option<Hob>,
    /// Log records below this level aren't reported.
    level: Level,
    blocks: BTreeMap<BlockId, Block>,
    islands: BTreeMap<IslandId, Island>,
    /// What happened since the last report.
    events: Vec<Event>,
    /// Islands to show in the next report.
    to_show: BTreeSet<IslandId>,
}

struct Block {
    line: String,
    finished: bool,
    /// Whether hob ended while it ran.
    interrupted: bool,
}

struct Island {
    block: BlockId,
    contents: FrozenIsland,
    live: bool,
    /// Whether it was printed frozen, rather than shown live.
    printed: bool,
    /// Whether a report has shown it in full.
    shown: bool,
}

/// Which changed islands a report shows in full. New ones are shown anyway.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    /// None.
    Nothing,
    /// Those of this block.
    Block(BlockId),
    /// All.
    All,
}

struct Event {
    block: Option<BlockId>,
    text: String,
    /// How many times it happened in a row.
    count: u32,
}

/// Why hob ended.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Why {
    /// Its output ended, without anyone asking it to exit.
    Unexpected,
    /// A command asked it to exit.
    Exit(BlockId),
    /// The agent closed it.
    Close,
}

/// What a tool returns: text for the agent, either way.
pub type ToolResult = Result<String, String>;

const NOT_RUNNING: &str = "error: hob isn't running; start starts it";

impl Bridge {
    /// A bridge that starts hob as `launch` says. Hob isn't started yet.
    pub fn new(launch: Launch, limits: Limits) -> Self {
        Bridge {
            launch,
            limits,
            hob: None,
            level: hob_core::logger::level_from_env(),
            blocks: BTreeMap::new(),
            islands: BTreeMap::new(),
            events: Vec::new(),
            to_show: BTreeSet::new(),
        }
    }

    /// Whether the agent can start hob again, and end it.
    pub fn can_restart(&self) -> bool {
        match self.launch {
            Launch::Command { restart, .. } => restart,
            Launch::ByAgent => true,
        }
    }

    /// Whether hob isn't running, and the agent can't start it again.
    pub fn is_over(&self) -> bool {
        self.hob.is_none() && !self.can_restart()
    }

    /// Starts hob, with `command` if it's started [by the
    /// agent](Launch::ByAgent), otherwise with the fixed command. Fails if
    /// it's running.
    pub fn start(&mut self, command: Option<Vec<OsString>>) -> ToolResult {
        self.apply_arrived();
        let command = match (&self.launch, command) {
            (Launch::Command { command, .. }, None) => command.clone(),
            (Launch::Command { .. }, Some(_)) => {
                let message = "error: hob-mcp starts hob with a fixed command; give none";
                return Err(self.report_with(message));
            }
            (Launch::ByAgent, Some(command)) => command,
            (Launch::ByAgent, None) => {
                return Err(self.report_with("error: give the command to start hob with"));
            }
        };
        if let Some(hob) = &self.hob {
            let message = format!("error: hob is already running (PID {})", hob.pid);
            return Err(self.report_with(&message));
        }
        let shown = (command.iter())
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        let hob = Hob::spawn(&command, self.limits.grace)
            .map_err(|e| self.report_with(&format!("error: couldn't start {shown}: {e}")))?;
        // Block and island numbers start over.
        self.blocks.clear();
        self.islands.clear();
        self.to_show.clear();
        self.event(None, format!("hob started (PID {}): {shown}", hob.pid));
        self.hob = Some(hob);
        // Report it if it exits right away, e.g. because of bad arguments.
        self.settle();
        match self.hob {
            Some(_) => Ok(self.report_with("")),
            None => Err(self.report_with("")),
        }
    }

    /// Closes hob, like closing its window: running commands are
    /// interrupted. Kills it if it doesn't exit.
    pub fn close(&mut self) -> ToolResult {
        self.apply_arrived();
        if self.hob.is_none() {
            return Err(self.report_with("error: hob isn't running"));
        }
        self.end(Why::Close);
        Ok(self.report_with(""))
    }

    /// Runs a command line, and waits until it finishes or shows an island.
    pub fn run(&mut self, line: &str, timeout: Duration) -> ToolResult {
        let call = Call::Run { line: line.into() };
        let result = self.call(call).map_err(|e| self.report_with(&e))?;
        let block = (result["block"].as_u64()).ok_or_else(|| self.report_with("bad reply"))?;
        let line = line.trim().to_owned();
        let (finished, interrupted) = (false, false);
        let block_state = Block {
            line,
            finished,
            interrupted,
        };
        self.blocks.insert(block, block_state);
        self.wait_for_input(block, timeout)
    }

    /// Fills in fields of a live island: text fields with strings, checkboxes
    /// with booleans.
    pub fn fill(
        &mut self,
        island: IslandId,
        fields: &serde_json::Map<String, Value>,
    ) -> ToolResult {
        self.apply_arrived();
        if self.hob.is_none() {
            return Err(self.report_with(NOT_RUNNING));
        }
        for (field, value) in fields {
            let call = match value {
                Value::String(text) => Call::TypeText {
                    island,
                    field: field.clone(),
                    text: text.clone(),
                },
                Value::Bool(checked) => Call::SetChecked {
                    island,
                    field: field.clone(),
                    checked: *checked,
                },
                _ => {
                    let message = format!(
                        "the value for {field:?} must be a string (for a text field) or a \
                         boolean (for a checkbox)"
                    );
                    return Err(self.report_with(&message));
                }
            };
            if let Err(e) = self.call(call) {
                let focus = self.island_focus(island);
                return Err(self.report(focus, &format!("{field:?}: {e}")));
            }
        }
        Ok(self.report(self.island_focus(island), ""))
    }

    /// Presses a button, and waits until the command finishes or shows an
    /// island.
    pub fn press(&mut self, island: IslandId, label: &str, timeout: Duration) -> ToolResult {
        let label = label.to_owned();
        let call = Call::Press { island, label };
        self.call(call).map_err(|e| self.report_with(&e))?;
        match self.islands.get(&island) {
            Some(island) => self.wait_for_input(island.block, timeout),
            None => Ok(self.report_with("")),
        }
    }

    /// What happened since the last report, then the commands still running
    /// and all live islands.
    pub fn state(&mut self) -> ToolResult {
        self.settle();
        // Live islands are all shown below; this adds printed ones.
        let to_show = std::mem::take(&mut self.to_show);
        let mut report = self.events_report();
        for id in to_show {
            if !self.islands[&id].live {
                report += &self.show_island(id);
            }
        }
        let Some(hob) = &self.hob else {
            report += "hob isn't running.\n";
            return Ok(report);
        };
        let _ = writeln!(report, "hob is running (PID {}).", hob.pid);
        let running: Vec<_> = (self.blocks.iter()).filter(|(_, b)| !b.finished).collect();
        if running.is_empty() {
            report += "No commands are running.\n";
        } else {
            report += "Running commands:\n";
            for (id, block) in running {
                let _ = writeln!(report, "  [{id}] {}", block.line);
            }
        }
        let live: Vec<_> = (self.islands.keys())
            .filter(|id| self.islands[id].live)
            .copied()
            .collect();
        if live.is_empty() {
            report += "No live islands.\n";
        }
        for id in live {
            report += &self.show_island(id);
        }
        Ok(report)
    }

    /// Waits until something happens (in `block`, if given), or until
    /// `block` finishes; at most for `timeout`.
    pub fn wait(
        &mut self,
        timeout: Duration,
        block: Option<BlockId>,
        finished: bool,
    ) -> ToolResult {
        self.apply_arrived();
        if self.hob.is_none() && self.events.is_empty() {
            return Err(self.report_with("error: hob isn't running"));
        }
        if let Some(block) = block
            && !self.blocks.contains_key(&block)
        {
            return Err(self.report_with(&format!("there's no command [{block}]")));
        }
        let done = match (block, finished) {
            (Some(block), true) => self.wait_until(timeout, |b| b.blocks[&block].finished),
            (None, true) => return Err(self.report_with("`finished` needs a block")),
            (block, false) => self.wait_until(timeout, |b| {
                (b.events.iter()).any(|e| block.is_none() || e.block == block)
            }),
        };
        let mut status = match done {
            true => String::new(),
            false => format!("Nothing to wait for came within {}.\n", seconds(timeout)),
        };
        status += &block.and_then(|b| self.block_status(b)).unwrap_or_default();
        let focus = block.map_or(Focus::All, Focus::Block);
        Ok(self.report(focus, status.trim_end()))
    }

    /// Calls the frontend. The error says what went wrong, for the agent.
    fn call(&mut self, call: Call) -> Result<Value, String> {
        let Some(hob) = &mut self.hob else {
            return Err(NOT_RUNNING.into());
        };
        match hob.call(&call, self.limits.call) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(format!("error: {}", error.message)),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Err(format!(
                "error: hob didn't reply within {}; it may be stuck (close ends it)",
                seconds(self.limits.call)
            )),
            Err(_) => {
                self.end(Why::Unexpected);
                Err("error: hob has exited".into())
            }
        }
    }

    /// Receives an event from hob, waiting at most for `timeout`. `None` if
    /// none came, or hob isn't running.
    fn receive(&self, timeout: Duration) -> Option<HobEvent> {
        self.hob.as_ref()?.events.recv_timeout(timeout).ok()
    }

    /// Stops hob, applies what it sent before it ended, and reports how it
    /// ended.
    fn end(&mut self, why: Why) {
        let Some(mut hob) = self.hob.take() else {
            return;
        };
        let ended = hob.stop(self.limits.grace);
        for event in hob.remaining_events() {
            self.apply(event);
        }
        for (&id, block) in &mut self.blocks {
            if !block.finished {
                block.finished = true;
                block.interrupted = why != Why::Exit(id);
            }
        }
        let interrupted: Vec<_> = (self.blocks.iter())
            .filter(|(_, b)| b.interrupted)
            .map(|(id, _)| *id)
            .collect();
        for id in interrupted {
            self.event(Some(id), format!("[{id}] was interrupted"));
        }
        for island in self.islands.values_mut() {
            island.live = false;
        }

        let pid = hob.pid;
        let status = match &ended.status {
            Ok(status) => status.to_string(),
            Err(e) => format!("couldn't get its exit status: {e}"),
        };
        let mut text = match (why, ended.killed) {
            (Why::Unexpected, false) => format!("hob (PID {pid}) exited unexpectedly ({status})"),
            (Why::Unexpected, true) => {
                format!("hob (PID {pid}) closed its output, so it was killed ({status})")
            }
            (_, false) => format!("hob (PID {pid}) exited ({status})"),
            (_, true) => format!(
                "hob (PID {pid}) didn't exit within {}, so it was killed ({status})",
                seconds(self.limits.grace)
            ),
        };
        let failed = !ended.status.as_ref().is_ok_and(|s| s.success());
        if (failed || why == Why::Unexpected) && !ended.stderr.is_empty() {
            text += "; the last lines of its stderr:";
            for line in &ended.stderr {
                let _ = write!(text, "\n    {line}");
            }
        }
        if !self.can_restart() {
            text += "\nhob-mcp exits, since it can't start hob again";
        }
        self.event(None, text);
    }

    /// Waits until the command of `block` finishes or has a live island.
    fn wait_for_input(&mut self, block: BlockId, timeout: Duration) -> ToolResult {
        let done = self.wait_until(timeout, |b| {
            b.finished(block) || b.live_island(block).is_some()
        });
        let status = match self.block_status(block) {
            Some(status) => status,
            None if !done => format!(
                "[{block}] is still running after {}; use wait.",
                seconds(timeout)
            ),
            None => String::new(),
        };
        Ok(self.report(Focus::Block(block), &status))
    }

    /// Whether the command of `block` was interrupted, has finished or waits
    /// for an island; `None` if it's just running.
    fn block_status(&self, block: BlockId) -> Option<String> {
        if self.blocks.get(&block).is_some_and(|b| b.interrupted) {
            Some(format!("[{block}] was interrupted."))
        } else if self.finished(block) {
            Some(format!("[{block}] has finished."))
        } else {
            (self.live_island(block))
                .map(|island| format!("[{block}] is waiting for island #{island}."))
        }
    }

    /// The focus for acting on `island`: its block.
    fn island_focus(&self, island: IslandId) -> Focus {
        match self.islands.get(&island) {
            Some(island) => Focus::Block(island.block),
            None => Focus::Nothing,
        }
    }

    fn finished(&self, block: BlockId) -> bool {
        self.blocks.get(&block).is_none_or(|b| b.finished)
    }

    /// The newest live island of a block.
    fn live_island(&self, block: BlockId) -> Option<IslandId> {
        (self.islands.iter().rev())
            .find(|(_, i)| i.live && i.block == block)
            .map(|(id, _)| *id)
    }

    /// Applies notifications until `done` returns true, at most for `timeout`;
    /// then lets things settle. Returns whether `done` returned true.
    fn wait_until(&mut self, timeout: Duration, done: impl Fn(&Self) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        let result = loop {
            self.apply_arrived();
            if done(self) {
                break true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.receive(left) {
                Some(event) => self.apply(event),
                None => break false,
            }
        };
        self.settle();
        result
    }

    /// Applies the notifications that have arrived, without waiting.
    fn apply_arrived(&mut self) {
        while let Some(event) = self.receive(Duration::ZERO) {
            self.apply(event);
        }
    }

    /// Applies notifications until none come for [`SETTLE`], at most for
    /// [`MAX_SETTLE`].
    fn settle(&mut self) {
        let deadline = Instant::now() + MAX_SETTLE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.receive(SETTLE.min(left)) {
                Some(event) => self.apply(event),
                None => return,
            }
        }
    }

    fn event(&mut self, block: Option<BlockId>, text: String) {
        if let Some(last) = self.events.last_mut()
            && last.block == block
            && last.text == text
        {
            last.count += 1;
            return;
        }
        let count = 1;
        self.events.push(Event { block, text, count });
    }

    fn apply(&mut self, event: HobEvent) {
        match event {
            HobEvent::Notification(n) => self.apply_notification(n),
            HobEvent::OutputEnded => self.end(Why::Unexpected),
        }
    }

    fn apply_notification(&mut self, notification: Notification) {
        match notification {
            Notification::Printed { block, text } => {
                for line in text.lines() {
                    self.event(Some(block), format!("[{block}] {line}"));
                }
            }
            Notification::Logged { block, record } => {
                if record.level <= self.level {
                    let source = block.map_or("hob".to_owned(), |b| b.to_string());
                    self.event(block, format!("[{source}] {record}"));
                }
            }
            Notification::Island {
                block,
                island,
                live,
                contents,
            } => {
                let text = match (self.islands.contains_key(&island), live) {
                    (false, true) => format!("[{block}] showed island #{island}"),
                    (false, false) => format!("[{block}] printed island #{island}"),
                    (true, true) => format!("[{block}] island #{island} changed"),
                    (true, false) => format!("[{block}] island #{island} closed"),
                };
                let (printed, shown) = match self.islands.get(&island) {
                    Some(old) => (old.printed, old.shown),
                    None => (!live, false),
                };
                self.to_show.insert(island);
                self.event(Some(block), text);
                let island_state = Island {
                    block,
                    contents,
                    live,
                    printed,
                    shown,
                };
                self.islands.insert(island, island_state);
            }
            Notification::Cleared { block } => {
                self.event(Some(block), format!("[{block}] cleared the scrollback"));
            }
            Notification::ExitRequested { block } => {
                if self.can_restart() {
                    self.event(Some(block), format!("[{block}] asked hob to exit"));
                    self.end(Why::Exit(block));
                } else {
                    // Then the agent couldn't start it again.
                    let text = format!("[{block}] asked to exit, which does nothing here");
                    self.event(Some(block), text);
                }
            }
            Notification::Finished { block } => {
                if let Some(b) = self.blocks.get_mut(&block) {
                    b.finished = true;
                }
                self.event(Some(block), format!("[{block}] finished"));
            }
        }
    }

    /// What happened since the last report, and `status`; changed islands
    /// aren't shown.
    fn report_with(&mut self, status: &str) -> String {
        self.report(Focus::Nothing, status)
    }

    /// What happened since the last report, the islands to show (by
    /// `focus`), and `status`. Changed islands that aren't shown are kept
    /// for a later report.
    fn report(&mut self, focus: Focus, status: &str) -> String {
        self.apply_arrived();
        let mut report = self.events_report();
        let to_show = std::mem::take(&mut self.to_show);
        for id in to_show {
            let island = &self.islands[&id];
            // Islands closed since they changed aren't worth showing.
            if !island.live && !island.printed {
                continue;
            }
            let wanted = match focus {
                Focus::Nothing => false,
                Focus::Block(block) => island.block == block,
                Focus::All => true,
            };
            if wanted || !island.shown {
                report += &self.show_island(id);
            } else {
                self.to_show.insert(id);
            }
        }
        if !status.is_empty() {
            report += status;
            report += "\n";
        }
        if report.is_empty() {
            report = "Nothing new.\n".into();
        }
        report
    }

    fn events_report(&mut self) -> String {
        let mut report = String::new();
        if self.events.is_empty() {
            return report;
        }
        report += "What happened:\n";
        for event in self.events.drain(..) {
            match event.count {
                1 => {
                    let _ = writeln!(report, "  {}", event.text);
                }
                n => {
                    let _ = writeln!(report, "  {} ({n} times)", event.text);
                }
            }
        }
        report
    }

    /// An island's outline, which then counts as shown.
    fn show_island(&mut self, id: IslandId) -> String {
        let island = self.islands.get_mut(&id).expect("known island");
        island.shown = true;
        let kind = if island.live { "live" } else { "frozen" };
        let mut text = format!("Island #{id} of [{}], {kind}:\n", island.block);
        for line in kettle_ui_headless::outline(&island.contents).lines() {
            let _ = writeln!(text, "  {line}");
        }
        text
    }
}

/// A duration as seconds, for messages.
fn seconds(duration: Duration) -> String {
    format!("{} s", duration.as_secs_f64())
}
