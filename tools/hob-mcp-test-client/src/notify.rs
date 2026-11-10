//! Notifications from servers: progress, log messages, list changes and
//! resource updates.
//!
//! A command waiting for the server registers as a waiter, and the latest
//! one gets the notifications that arrive meanwhile, to show in its block.
//! Progress goes there too: rmcp gives every request a progress token of
//! its own (replacing any the client sets), and its request methods don't
//! say which, so progress can't be matched to its request. Updates of a
//! resource go to the command subscribed to it. What no command takes
//! is logged (target [`TARGET`]): warnings and errors show in the
//! scrollback, the rest in `messages`.

// MCP's logging is deprecated from 2026-07-28 (SEP-2577), but servers
// still use it with earlier versions, which the client speaks.
#![expect(deprecated, reason = "MCP logging is used before 2026-07-28")]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use hob_core::Context;
use rmcp::ClientHandler;
use rmcp::RoleClient;
use rmcp::model::{
    CancelledNotificationParam, ClientConfig, CustomNotification, LoggingLevel,
    LoggingMessageNotificationParam, ProgressNotificationParam, ResourceUpdatedNotificationParam,
};
use rmcp::service::{NotificationContext, Peer};
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::{Names, client_config, lock};

/// The log target of notifications no command takes.
pub(crate) const TARGET: &str = "hob_mcp_test_client::notification";

/// What the client and its connections' handlers share.
#[derive(Default)]
pub(crate) struct Shared {
    /// What the server offers, as last listed, for completion.
    pub(crate) names: Mutex<Names>,
    waiters: Mutex<Vec<Waiter>>,
    next_key: AtomicU64,
}

struct Waiter {
    key: u64,
    /// The connection whose notifications it gets.
    connection: u64,
    kind: Waiting,
    sender: UnboundedSender<Event>,
}

/// What a command waits for.
pub(crate) enum Waiting {
    /// The response to a request.
    Request,
    /// Updates of the resource with this URI.
    Resource(String),
}

/// A notification, as a waiting command gets it.
pub(crate) enum Event {
    Progress(ProgressNotificationParam),
    /// Something to log in the command's block.
    Message(log::Level, String),
    /// The resource waited for was updated.
    Updated,
    /// The connection has closed.
    Closed,
}

/// Registers a waiter until dropped.
pub(crate) struct Registration {
    shared: Arc<Shared>,
    key: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        lock(&self.shared.waiters).retain(|waiter| waiter.key != self.key);
    }
}

impl Shared {
    /// Registers a waiter for notifications of `connection`; they arrive
    /// on the receiver until the registration is dropped.
    pub(crate) fn wait(
        self: &Arc<Self>,
        connection: u64,
        kind: Waiting,
    ) -> (Registration, UnboundedReceiver<Event>) {
        let (sender, receiver) = unbounded_channel();
        let key = self.next_key.fetch_add(1, Ordering::Relaxed);
        lock(&self.waiters).push(Waiter {
            key,
            connection,
            kind,
            sender,
        });
        let registration = Registration {
            shared: self.clone(),
            key,
        };
        (registration, receiver)
    }

    /// Tells the waiters of `connection` that it has closed.
    pub(crate) fn closed(&self, connection: u64) {
        for waiter in lock(&self.waiters).iter() {
            if waiter.connection == connection {
                let _ = waiter.sender.send(Event::Closed);
            }
        }
    }

    /// Sends `event` to the latest command waiting for a response from
    /// `connection`. Gives it back if there's none.
    fn to_waiter(&self, connection: u64, event: Event) -> Result<(), Event> {
        let waiters = lock(&self.waiters);
        let waiter = waiters.iter().rev().find(|waiter| {
            waiter.connection == connection && matches!(waiter.kind, Waiting::Request)
        });
        match waiter {
            Some(waiter) => waiter.sender.send(event).map_err(|e| e.0),
            None => Err(event),
        }
    }

    fn progress(&self, connection: u64, params: ProgressNotificationParam) {
        if let Err(Event::Progress(params)) = self.to_waiter(connection, Event::Progress(params)) {
            // E.g. after the response.
            log::debug!(target: TARGET, "{}", progress_text(&params));
        }
    }

    fn resource_updated(&self, connection: u64, uri: String) {
        let waiters = lock(&self.waiters);
        let mut taken = false;
        for waiter in waiters.iter() {
            if waiter.connection == connection
                && matches!(&waiter.kind, Waiting::Resource(waited) if *waited == uri)
            {
                taken |= waiter.sender.send(Event::Updated).is_ok();
            }
        }
        drop(waiters);
        if !taken {
            self.message(connection, log::Level::Info, format!("Updated: {uri}"));
        }
    }

    /// Sends a message to the latest command waiting for a response from
    /// `connection`, or logs it if there's none.
    fn message(&self, connection: u64, level: log::Level, text: String) {
        let event = Event::Message(level, text);
        if let Err(Event::Message(level, text)) = self.to_waiter(connection, event) {
            log::log!(target: TARGET, level, "{text}");
        }
    }
}

/// Handles a connection's notifications, and introduces the client.
pub(crate) struct Handler {
    pub(crate) shared: Arc<Shared>,
    pub(crate) connection: u64,
}

/// A list a server may say has changed.
#[derive(Clone, Copy)]
enum List {
    Tools,
    Resources,
    Prompts,
}

impl Handler {
    /// Reports a changed list, and lists it again for completion. In a task
    /// of its own: the connection's handler can't wait for a response.
    fn list_changed(&self, list: List, peer: Peer<RoleClient>) {
        let what = match list {
            List::Tools => "tools",
            List::Resources => "resources",
            List::Prompts => "prompts",
        };
        let text = format!("The server's {what} have changed.");
        self.shared.message(self.connection, log::Level::Info, text);
        let shared = self.shared.clone();
        tokio::spawn(async move {
            let names: Result<Vec<String>, _> = match list {
                List::Tools => (peer.list_all_tools().await)
                    .map(|tools| tools.into_iter().map(|t| t.name.to_string()).collect()),
                List::Resources => (peer.list_all_resources().await)
                    .map(|resources| resources.into_iter().map(|r| r.uri).collect()),
                List::Prompts => (peer.list_all_prompts().await)
                    .map(|prompts| prompts.into_iter().map(|p| p.name).collect()),
            };
            let Ok(names) = names else { return };
            let mut all = lock(&shared.names);
            match list {
                List::Tools => all.tools = names,
                List::Resources => all.resources = names,
                List::Prompts => all.prompts = names,
            }
        });
    }
}

impl ClientHandler for Handler {
    fn get_info(&self) -> ClientConfig {
        client_config()
    }

    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _: NotificationContext<RoleClient>,
    ) {
        self.shared.progress(self.connection, params);
    }

    async fn on_logging_message(
        &self,
        params: LoggingMessageNotificationParam,
        _: NotificationContext<RoleClient>,
    ) {
        let (level, text) = log_message(&params);
        self.shared.message(self.connection, level, text);
    }

    async fn on_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
        _: NotificationContext<RoleClient>,
    ) {
        self.shared.resource_updated(self.connection, params.uri);
    }

    async fn on_tool_list_changed(&self, context: NotificationContext<RoleClient>) {
        self.list_changed(List::Tools, context.peer);
    }

    async fn on_resource_list_changed(&self, context: NotificationContext<RoleClient>) {
        self.list_changed(List::Resources, context.peer);
    }

    async fn on_prompt_list_changed(&self, context: NotificationContext<RoleClient>) {
        self.list_changed(List::Prompts, context.peer);
    }

    async fn on_cancelled(
        &self,
        params: CancelledNotificationParam,
        _: NotificationContext<RoleClient>,
    ) {
        let mut text = "The server cancelled a request".to_owned();
        if let Some(id) = &params.request_id {
            text += &format!(" ({id})");
        }
        if let Some(reason) = &params.reason {
            text += &format!(": {reason}");
        }
        self.shared.message(self.connection, log::Level::Info, text);
    }

    async fn on_custom_notification(
        &self,
        notification: CustomNotification,
        _: NotificationContext<RoleClient>,
    ) {
        let mut text = format!("Notification {}", notification.method);
        if let Some(params) = &notification.params {
            text += &format!(": {params}");
        }
        self.shared.message(self.connection, log::Level::Info, text);
    }
}

/// A server's log message, at the closest level: the logger's name, if
/// any, then the data (text as it is, anything else as JSON).
fn log_message(params: &LoggingMessageNotificationParam) -> (log::Level, String) {
    let level = match params.level {
        LoggingLevel::Debug => log::Level::Debug,
        LoggingLevel::Info | LoggingLevel::Notice => log::Level::Info,
        LoggingLevel::Warning => log::Level::Warn,
        _ => log::Level::Error,
    };
    let data = match &params.data {
        Value::String(text) => text.clone(),
        data => data.to_string(),
    };
    let text = match &params.logger {
        Some(logger) => format!("{logger}: {data}"),
        None => data,
    };
    (level, text)
}

/// A progress notification as one line, e.g. "Progress: 3 of 10: Copying".
fn progress_text(params: &ProgressNotificationParam) -> String {
    let mut text = format!("Progress: {}", params.progress);
    if let Some(total) = params.total {
        text += &format!(" of {total}");
    }
    if let Some(message) = &params.message {
        text += &format!(": {message}");
    }
    text
}

/// Shows `first` and the events already queued after it, in a command's
/// block. Of several progress notifications in a row, only the last is
/// shown, so a server sending many doesn't flood the block.
pub(crate) fn show_events(ctx: &Context, first: Event, events: &mut UnboundedReceiver<Event>) {
    let mut batch = vec![first];
    while let Ok(event) = events.try_recv() {
        batch.push(event);
    }
    let mut batch = batch.into_iter().peekable();
    while let Some(event) = batch.next() {
        match event {
            Event::Progress(params) => {
                if !matches!(batch.peek(), Some(Event::Progress(_))) {
                    ctx.print(progress_text(&params));
                }
            }
            Event::Message(level, text) => match level {
                log::Level::Error => ctx.error(text),
                log::Level::Warn => ctx.warn(text),
                log::Level::Info => ctx.info(text),
                log::Level::Debug | log::Level::Trace => ctx.debug(text),
            },
            Event::Updated | Event::Closed => {}
        }
    }
}
