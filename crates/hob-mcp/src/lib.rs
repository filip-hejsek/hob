//! An MCP server that lets an agent use hob: a bridge to the headless
//! frontend (`hob --headless`).
//!
//! The agent runs commands and acts on their islands with tools (see
//! [`mcp`]). Hob doesn't wait for the agent: commands print, islands change
//! and commands finish in the meantime. All of that is collected, and every
//! tool result starts with what happened since the last one, so the agent
//! never misses anything. The `wait` tool waits for more.

pub mod mcp;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use hob_core::Level;
use hob_headless::client::Client;
use hob_headless::protocol::{BlockId, Call, IslandId, Notification};
use kettle_ui::FrozenIsland;
use serde_json::Value;

/// After something happens, how long to wait for more before reporting, so
/// that things that happen together are reported together.
const SETTLE: Duration = Duration::from_millis(100);

/// The longest a report waits for things to settle.
const MAX_SETTLE: Duration = Duration::from_millis(500);

/// Hob, as the agent sees it.
pub struct Bridge {
    client: Client,
    notifications: Receiver<Notification>,
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
}

struct Island {
    block: BlockId,
    contents: FrozenIsland,
    live: bool,
    /// Whether it was printed frozen, rather than shown live.
    printed: bool,
}

struct Event {
    block: Option<BlockId>,
    text: String,
    /// How many times it happened in a row.
    count: u32,
}

/// What a tool returns: text for the agent, either way.
pub type ToolResult = Result<String, String>;

impl Bridge {
    /// A bridge to the frontend behind `client`, which passes its
    /// notifications to `notifications`.
    pub fn new(client: Client, notifications: Receiver<Notification>) -> Self {
        Bridge {
            client,
            notifications,
            level: hob_core::logger::level_from_env(),
            blocks: BTreeMap::new(),
            islands: BTreeMap::new(),
            events: Vec::new(),
            to_show: BTreeSet::new(),
        }
    }

    /// Runs a command line, and waits until it finishes or shows an island.
    pub fn run(&mut self, line: &str, timeout: Duration) -> ToolResult {
        let call = Call::Run { line: line.into() };
        let result = self.call(call).map_err(|e| self.report_with(&e))?;
        let block = (result["block"].as_u64()).ok_or_else(|| self.report_with("bad reply"))?;
        let line = line.trim().to_owned();
        let finished = false;
        self.blocks.insert(block, Block { line, finished });
        self.wait_for_input(block, timeout)
    }

    /// Fills in fields of a live island: text fields with strings, checkboxes
    /// with booleans.
    pub fn fill(
        &mut self,
        island: IslandId,
        fields: &serde_json::Map<String, Value>,
    ) -> ToolResult {
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
                return Err(self.report_with(&format!("{field:?}: {e}")));
            }
        }
        Ok(self.report_with(""))
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
        let mut report = self.events_report();
        self.to_show.clear();
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
            .collect();
        if live.is_empty() {
            report += "No live islands.\n";
        }
        for id in live {
            report += &self.show_island(*id);
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
        let status = match done {
            true => String::new(),
            false => format!("Nothing to wait for came within {}.", seconds(timeout)),
        };
        Ok(self.report_with(&status))
    }

    /// Calls the frontend. The error says what went wrong, for the agent.
    fn call(&mut self, call: Call) -> Result<Value, String> {
        match self.client.call(&call) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(format!("error: {}", error.message)),
            Err(e) => Err(format!("error: hob's headless frontend is gone: {e}")),
        }
    }

    /// Waits until the command of `block` finishes or has a live island.
    fn wait_for_input(&mut self, block: BlockId, timeout: Duration) -> ToolResult {
        let done = self.wait_until(timeout, |b| {
            b.finished(block) || b.live_island(block).is_some()
        });
        let status = if self.finished(block) {
            format!("[{block}] has finished.")
        } else if let Some(island) = self.live_island(block) {
            format!("[{block}] is waiting for island #{island}.")
        } else if !done {
            format!(
                "[{block}] is still running after {}; use wait.",
                seconds(timeout)
            )
        } else {
            String::new()
        };
        Ok(self.report_with(&status))
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
            match self.notifications.recv_timeout(left) {
                Ok(n) => self.apply(n),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break false,
            }
        };
        self.settle();
        result
    }

    /// Applies the notifications that have arrived, without waiting.
    fn apply_arrived(&mut self) {
        while let Ok(n) = self.notifications.try_recv() {
            self.apply(n);
        }
    }

    /// Applies notifications until none come for [`SETTLE`], at most for
    /// [`MAX_SETTLE`].
    fn settle(&mut self) {
        let deadline = Instant::now() + MAX_SETTLE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.notifications.recv_timeout(SETTLE.min(left)) {
                Ok(n) => self.apply(n),
                Err(_) => return,
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

    fn apply(&mut self, notification: Notification) {
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
                let printed = match self.islands.get(&island) {
                    Some(old) => old.printed,
                    None => !live,
                };
                self.to_show.insert(island);
                self.event(Some(block), text);
                let island_state = Island {
                    block,
                    contents,
                    live,
                    printed,
                };
                self.islands.insert(island, island_state);
            }
            Notification::Cleared { block } => {
                self.event(Some(block), format!("[{block}] cleared the scrollback"));
            }
            Notification::Finished { block } => {
                if let Some(b) = self.blocks.get_mut(&block) {
                    b.finished = true;
                }
                self.event(Some(block), format!("[{block}] finished"));
            }
        }
    }

    /// What happened since the last report, and `status`.
    fn report_with(&mut self, status: &str) -> String {
        self.apply_arrived();
        let mut report = self.events_report();
        // Islands closed since they changed aren't worth showing.
        let to_show = std::mem::take(&mut self.to_show);
        for id in to_show {
            let island = &self.islands[&id];
            if island.live || island.printed {
                report += &self.show_island(id);
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

    fn show_island(&self, id: IslandId) -> String {
        let island = &self.islands[&id];
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
