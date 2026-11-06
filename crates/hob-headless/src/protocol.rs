//! The protocol between the headless frontend and its client.
//!
//! It's [JSON-RPC 2.0](https://www.jsonrpc.org/specification), one message
//! per line, over the frontend's stdin (from the client) and stdout (to the
//! client). The client calls methods ([`Call`]); the frontend replies, and
//! sends [`Notification`]s about what commands do. Notifications caused by a
//! call (e.g. an island changing after `type_text`) come before its reply.
//!
//! ```text
//! → {"jsonrpc":"2.0","id":1,"method":"run","params":{"line":"echo hi"}}
//! ← {"jsonrpc":"2.0","id":1,"result":{"block":1}}
//! ← {"jsonrpc":"2.0","method":"printed","params":{"block":1,"text":"hi"}}
//! ← {"jsonrpc":"2.0","method":"finished","params":{"block":1}}
//! ```
//!
//! Like in the GUI, each command line gets a block, and everything the
//! command does refers to it. Islands are sent as frozen islands (the format
//! of saved sessions): the widget tree with field values and state. A live
//! island is sent again after each change, and once more when it's frozen
//! (`"live": false`), after the user submits or cancels it.

use std::fmt;

use hob_core::LogRecord;
use kettle_ui::FrozenIsland;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Identifies a command line's block.
pub type BlockId = u64;

/// Identifies an island.
pub type IslandId = u64;

/// A method call from the client: the method name and its parameters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Call {
    /// Runs a command line. Replies with a [`RunResult`].
    Run {
        /// The command line.
        line: String,
    },
    /// Replaces the text of a text field in a live island.
    TypeText {
        /// The island.
        island: IslandId,
        /// The text field's id.
        field: String,
        /// The new text.
        text: String,
    },
    /// Checks or unchecks a checkbox in a live island.
    SetChecked {
        /// The island.
        island: IslandId,
        /// The checkbox's id.
        field: String,
        /// Whether it's checked.
        checked: bool,
    },
    /// Picks one of the suggestions shown for a text field.
    Pick {
        /// The island.
        island: IslandId,
        /// The text field the suggestions are for.
        field: String,
        /// The suggestion, which must be shown.
        line: String,
    },
    /// Presses a button, by its label.
    Press {
        /// The island.
        island: IslandId,
        /// The button's label.
        label: String,
    },
}

impl Call {
    /// The names of all methods.
    pub const METHODS: &[&str] = &["run", "type_text", "set_checked", "pick", "press"];
}

/// The reply to [`Call::Run`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunResult {
    /// The command line's block.
    pub block: BlockId,
}

/// Something the frontend tells the client, without being asked.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Notification {
    /// A command printed text.
    Printed {
        /// The command's block.
        block: BlockId,
        /// The text; it may have several lines.
        text: String,
    },
    /// A log record, from a command (with its block) or from elsewhere.
    Logged {
        /// The command's block, if it's from a command.
        block: Option<BlockId>,
        /// The record.
        record: LogRecord,
    },
    /// A command showed or printed an island, or a live island changed.
    Island {
        /// The command's block.
        block: BlockId,
        /// The island.
        island: IslandId,
        /// Whether it's live: false once it's frozen, or if it was printed
        /// frozen. A frozen island never changes again.
        live: bool,
        /// What the island shows.
        contents: FrozenIsland,
    },
    /// A command finished; nothing more will refer to its block.
    Finished {
        /// The command's block.
        block: BlockId,
    },
}

/// A JSON-RPC error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorObject {
    /// What kind of error it is; see [`code`].
    pub code: i64,
    /// Says what went wrong.
    pub message: String,
}

impl ErrorObject {
    /// An error with this code and message.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        ErrorObject {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for ErrorObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

impl std::error::Error for ErrorObject {}

/// Error codes: JSON-RPC's own, and ours.
pub mod code {
    /// The message isn't JSON.
    pub const PARSE_ERROR: i64 = -32700;
    /// The message isn't a JSON-RPC request.
    pub const INVALID_REQUEST: i64 = -32600;
    /// There's no method with this name.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// The parameters don't fit the method.
    pub const INVALID_PARAMS: i64 = -32602;
    /// The command line couldn't be started, e.g. the command doesn't exist.
    pub const NOT_STARTED: i64 = -32001;
    /// There's no live island with this id.
    pub const NO_LIVE_ISLAND: i64 = -32002;
    /// The action isn't possible in the island, e.g. there's no such button.
    pub const ACTION_FAILED: i64 = -32003;
}

/// A message to the client, as parsed by [`parse_incoming`].
pub enum Incoming {
    /// The reply to a call.
    Response {
        /// The id of the call.
        id: Value,
        /// The result, or the error.
        result: Result<Value, ErrorObject>,
    },
    /// A notification.
    Notification(Notification),
}

/// A call that couldn't be parsed; see [`parse_call`].
#[derive(Debug)]
pub struct BadCall {
    /// The id to reply to; `None` if no reply should be sent (the call was a
    /// notification).
    pub id: Option<Value>,
    /// What's wrong.
    pub error: ErrorObject,
}

/// A request line for a call.
pub fn request(id: u64, call: &Call) -> String {
    let mut message = serde_json::to_value(call).expect("calls serialize");
    message["jsonrpc"] = "2.0".into();
    message["id"] = id.into();
    message.to_string()
}

/// A line for a notification.
pub fn notification(notification: &Notification) -> String {
    let mut message = serde_json::to_value(notification).expect("notifications serialize");
    message["jsonrpc"] = "2.0".into();
    message.to_string()
}

/// A response line.
pub fn response(id: &Value, result: &Result<Value, ErrorObject>) -> String {
    match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
    }
    .to_string()
}

/// Parses a line from the client. Returns the call and its id; the id is
/// `None` for a notification, which gets no reply.
pub fn parse_call(line: &str) -> Result<(Option<Value>, Call), BadCall> {
    let message: Value = serde_json::from_str(line).map_err(|e| BadCall {
        id: Some(Value::Null),
        error: ErrorObject::new(code::PARSE_ERROR, e.to_string()),
    })?;
    let id = message.get("id").cloned();
    let invalid = |message: &str| BadCall {
        id: Some(Value::Null),
        error: ErrorObject::new(code::INVALID_REQUEST, message),
    };
    let Value::Object(mut object) = message else {
        return Err(invalid("not an object"));
    };
    if object.get("jsonrpc") != Some(&Value::from("2.0")) {
        return Err(invalid(r#"expected "jsonrpc": "2.0""#));
    }
    let method = match object.get("method") {
        Some(Value::String(method)) => method.clone(),
        _ => return Err(invalid("no method")),
    };
    if !Call::METHODS.contains(&method.as_str()) {
        return Err(BadCall {
            id,
            error: ErrorObject::new(code::METHOD_NOT_FOUND, format!("no method {method:?}")),
        });
    }
    let params = object.remove("params").unwrap_or(Value::Null);
    match serde_json::from_value(json!({"method": method, "params": params})) {
        Ok(call) => Ok((id, call)),
        Err(e) => Err(BadCall {
            id,
            error: ErrorObject::new(code::INVALID_PARAMS, e.to_string()),
        }),
    }
}

/// Parses a line from the frontend.
pub fn parse_incoming(line: &str) -> Result<Incoming, serde_json::Error> {
    let mut message: Value = serde_json::from_str(line)?;
    if let Some(id) = message.get("id").cloned() {
        let result = match message.get_mut("error") {
            Some(error) => Err(serde_json::from_value(error.take())?),
            None => Ok(message.get_mut("result").map_or(Value::Null, Value::take)),
        };
        return Ok(Incoming::Response { id, result });
    }
    let method = message.get_mut("method").map(Value::take);
    let params = message.get_mut("params").map(Value::take);
    let notification = json!({"method": method, "params": params});
    Ok(Incoming::Notification(serde_json::from_value(
        notification,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calls() -> Vec<Call> {
        let island = 3;
        let field = || "f".to_owned();
        vec![
            Call::Run { line: "x".into() },
            Call::TypeText {
                island,
                field: field(),
                text: "t".into(),
            },
            Call::SetChecked {
                island,
                field: field(),
                checked: true,
            },
            Call::Pick {
                island,
                field: field(),
                line: "l".into(),
            },
            Call::Press {
                island,
                label: "OK".into(),
            },
        ]
    }

    #[test]
    fn calls_round_trip_and_methods_are_listed() {
        let calls = calls();
        assert_eq!(calls.len(), Call::METHODS.len());
        for (i, call) in calls.into_iter().enumerate() {
            let line = request(i as u64, &call);
            let (id, parsed) = parse_call(&line).unwrap();
            assert_eq!((id, &parsed), (Some(Value::from(i)), &call));
            let method = serde_json::from_str::<Value>(&line).unwrap()["method"].clone();
            assert_eq!(method, Call::METHODS[i]);
        }
    }

    #[test]
    fn bad_calls_get_json_rpc_errors() {
        let error = |line: &str| {
            let bad = parse_call(line).unwrap_err();
            (bad.id, bad.error.code)
        };
        assert_eq!(error("{"), (Some(Value::Null), code::PARSE_ERROR));
        assert_eq!(error("[]"), (Some(Value::Null), code::INVALID_REQUEST));
        assert_eq!(
            error(r#"{"id":1,"method":"run","params":{"line":"x"}}"#),
            (Some(Value::Null), code::INVALID_REQUEST)
        );
        assert_eq!(
            error(r#"{"jsonrpc":"2.0","id":1,"method":"fly"}"#),
            (Some(1.into()), code::METHOD_NOT_FOUND)
        );
        assert_eq!(
            error(r#"{"jsonrpc":"2.0","id":"a","method":"run","params":{}}"#),
            (Some("a".into()), code::INVALID_PARAMS)
        );
        // A notification gets no reply, even if it's bad.
        assert_eq!(
            error(r#"{"jsonrpc":"2.0","method":"fly"}"#),
            (None, code::METHOD_NOT_FOUND)
        );
    }

    #[test]
    fn incoming_messages_parse() {
        let line = response(&7.into(), &Ok(json!({"block": 2})));
        let Ok(Incoming::Response { id, result }) = parse_incoming(&line) else {
            panic!("expected a response");
        };
        assert_eq!((id, result), (7.into(), Ok(json!({"block": 2}))));

        let error = ErrorObject::new(code::NO_LIVE_ISLAND, "gone");
        let line = response(&7.into(), &Err(error.clone()));
        let Ok(Incoming::Response { result, .. }) = parse_incoming(&line) else {
            panic!("expected a response");
        };
        assert_eq!(result, Err(error));

        let line = notification(&Notification::Finished { block: 2 });
        assert_eq!(
            line,
            r#"{"jsonrpc":"2.0","method":"finished","params":{"block":2}}"#
        );
        let Ok(Incoming::Notification(Notification::Finished { block })) = parse_incoming(&line)
        else {
            panic!("expected a notification");
        };
        assert_eq!(block, 2);
    }
}
