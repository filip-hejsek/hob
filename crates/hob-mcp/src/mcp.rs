//! The MCP side: JSON-RPC 2.0 over stdio, one message per line, with tools
//! that call the [`Bridge`].
//!
//! Only what tools need is implemented: `initialize`, `ping`, `tools/list`
//! and `tools/call`. Notifications from the client are ignored.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::{Bridge, Launch};

/// The protocol versions this server knows, newest first. It speaks the
/// client's if it's one of them, otherwise the newest.
const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// How long `run`, `press` and `wait` wait by default.
const DEFAULT_TIMEOUT: f64 = 10.0;

/// The longest any tool waits.
const MAX_TIMEOUT: f64 = 300.0;

const INSTRUCTIONS: &str = "\
Hob runs commands, like a terminal. A command prints text and shows islands: \
small forms that it waits on until a button submits or cancels them. Each \
command line you run gets a number, its block, shown like [3]; everything a \
command does is reported with it. Run `help` to list the commands.

Islands are shown as outlines, one widget per line, e.g.:
  text_field name: \"Ada\" (placeholder \"Your name\")
  checkbox subscribe: [x] \"Subscribe\"
  button \"Submit\"
Fill in text fields and checkboxes by their ids (here name and subscribe) \
with fill, and press buttons by their labels with press. A button may show \
its keyboard key after the label, as in `button \"Yes\" [y]`; its label is \
still just Yes. Labels and lists update as you fill fields in.

Commands keep running between tool calls. Every tool result starts with what \
happened since the last one, and shows islands that are new or changed. Use \
wait to wait for more, and state for an overview.";

/// Added to [`INSTRUCTIONS`] when the agent starts hob.
const BY_AGENT: &str = "

Hob isn't running at first: start it with start, giving the command that \
runs its headless frontend. It stops when a command exits it (e.g. exit), \
when you close it, or if it crashes or is killed; results report that, with \
its exit status. Then start it again; block and island numbers start over.";

/// Added to [`INSTRUCTIONS`] when hob-mcp starts hob with a fixed command,
/// and may start it again.
const FIXED: &str = "

Hob stops when a command exits it (e.g. exit), when you close it, or if it \
crashes; results report that. start starts it again; block and island \
numbers start over.";

/// Serves MCP over `input` and `output`, until `input` ends, or hob has
/// exited and can't be started again ([`Bridge::is_over`]); the reply that
/// reports that is sent first.
pub fn serve(bridge: &mut Bridge, input: impl Read, mut output: impl Write) -> io::Result<()> {
    for line in BufReader::new(input).lines() {
        if let Some(reply) = bridge.handle_message(&line?) {
            writeln!(output, "{reply}")?;
            output.flush()?;
        }
        if bridge.is_over() {
            break;
        }
    }
    Ok(())
}

impl Bridge {
    /// Handles a JSON-RPC message from the MCP client; returns the reply, if
    /// there is one.
    pub fn handle_message(&mut self, line: &str) -> Option<String> {
        if line.trim().is_empty() {
            return None;
        }
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(e) => return Some(error(&Value::Null, -32700, &e.to_string())),
        };
        // Notifications, and replies to requests (this server sends none),
        // get no reply.
        let (Some(id), Some(method)) = (message.get("id"), message.get("method")) else {
            return None;
        };
        let params = message.get("params").cloned().unwrap_or(json!({}));
        let result = match method.as_str().unwrap_or("") {
            "initialize" => Ok(initialize(&params, &self.launch, self.can_restart())),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools(&self.launch, self.can_restart()) })),
            "tools/call" => self.call_tool(&params),
            method => Err((-32601, format!("no method {method:?}"))),
        };
        Some(match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
            Err((code, message)) => error(id, code, &message),
        })
    }

    fn call_tool(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params["name"].as_str().unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let result = match name {
            "run" => args_for(args).and_then(|a: RunArgs| self.run(&a.line, timeout(a.timeout))),
            "fill" => args_for(args).and_then(|a: FillArgs| self.fill(a.island, &a.fields)),
            "press" => args_for(args)
                .and_then(|a: PressArgs| self.press(a.island, &a.label, timeout(a.timeout))),
            "state" => args_for(args).and_then(|NoArgs {}| self.state()),
            "wait" => args_for(args).and_then(|a: WaitArgs| {
                let finished = matches!(a.until.as_deref(), Some("finished"));
                self.wait(timeout(a.timeout), a.block, finished)
            }),
            "start" | "close" if !self.can_restart() => {
                return Err((-32602, format!("no tool {name:?}")));
            }
            "start" => args_for(args).and_then(|a: StartArgs| {
                self.start(a.command.map(|c| c.into_iter().map(Into::into).collect()))
            }),
            "close" => args_for(args).and_then(|NoArgs {}| self.close()),
            name => return Err((-32602, format!("no tool {name:?}"))),
        };
        let (text, is_error) = match result {
            Ok(text) => (text, false),
            Err(text) => (text, true),
        };
        Ok(json!({
            "content": [{"type": "text", "text": text}],
            "isError": is_error,
        }))
    }
}

fn error(id: &Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

fn initialize(params: &Value, launch: &Launch, restart: bool) -> Value {
    let requested = params["protocolVersion"].as_str().unwrap_or("");
    let version = match VERSIONS.contains(&requested) {
        true => requested,
        false => VERSIONS[0],
    };
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "hob", "version": env!("CARGO_PKG_VERSION")},
        "instructions": match launch {
            Launch::Command { .. } if restart => format!("{INSTRUCTIONS}{FIXED}"),
            Launch::Command { .. } => INSTRUCTIONS.to_owned(),
            Launch::ByAgent => format!("{INSTRUCTIONS}{BY_AGENT}"),
        },
    })
}

fn args_for<T: DeserializeOwned>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|e| format!("bad arguments: {e}"))
}

fn timeout(seconds: Option<f64>) -> Duration {
    let seconds = seconds.unwrap_or(DEFAULT_TIMEOUT);
    // JSON has no NaN, which `clamp` would pass through.
    Duration::from_secs_f64(seconds.clamp(0.0, MAX_TIMEOUT))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    line: String,
    #[serde(rename = "timeout_seconds")]
    timeout: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FillArgs {
    island: u64,
    fields: Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PressArgs {
    island: u64,
    label: String,
    #[serde(rename = "timeout_seconds")]
    timeout: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    block: Option<u64>,
    until: Option<String>,
    #[serde(rename = "timeout_seconds")]
    timeout: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartArgs {
    command: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

/// The tool definitions for `tools/list`.
fn tools(launch: &Launch, restart: bool) -> Value {
    let start = match launch {
        Launch::Command { .. } => json!({
            "name": "start",
            "description": "Start hob again, after it has exited. Reports its PID.",
            "inputSchema": {"type": "object", "properties": {}},
        }),
        Launch::ByAgent => json!({
            "name": "start",
            "description": "Start hob with a command, which must run hob's headless \
                frontend, e.g. [\"target/debug/hob\", \"--headless\"] (build it first). \
                Reports its PID, and what it did. Only one hob runs at a time.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "array",
                        "items": {"type": "string"},
                        "minItems": 1,
                        "description": "The program and its arguments; no shell is used.",
                    },
                },
                "required": ["command"],
            },
        }),
    };
    let timeout = |what: &str| {
        json!({
            "type": "number",
            "description": format!("How long to wait for {what}, in seconds (default 10)."),
        })
    };
    let mut tools = json!([
        {
            "name": "run",
            "description": "Run a hob command line, like typing it at hob's prompt. Waits \
                until the command finishes or shows an island, then reports what happened.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "line": {"type": "string", "description": "The command line, e.g. \"help\"."},
                    "timeout_seconds": timeout("the command"),
                },
                "required": ["line"],
            },
        },
        {
            "name": "fill",
            "description": "Fill in fields of a live island: text fields (the text replaces \
                what's there) and checkboxes. Reports the island as it is afterwards.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "island": {"type": "integer", "description": "The island's number."},
                    "fields": {
                        "type": "object",
                        "description": "Field ids mapped to values: a string for a text \
                            field, true or false for a checkbox.",
                        "additionalProperties": {"type": ["string", "boolean"]},
                    },
                },
                "required": ["island", "fields"],
            },
        },
        {
            "name": "press",
            "description": "Press a button of a live island, by its label. Waits until the \
                command finishes or shows an island, then reports what happened.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "island": {"type": "integer", "description": "The island's number."},
                    "label": {"type": "string", "description": "The button's label."},
                    "timeout_seconds": timeout("the command"),
                },
                "required": ["island", "label"],
            },
        },
        {
            "name": "state",
            "description": "Report what happened since the last tool call, the commands \
                still running, and all live islands.",
            "inputSchema": {"type": "object", "properties": {}},
        },
        {
            "name": "wait",
            "description": "Wait until something happens (a command prints, an island \
                changes, a command finishes), or until a command finishes, then report \
                what happened.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "block": {
                        "type": "integer",
                        "description": "Only wait for this command (its block number).",
                    },
                    "until": {
                        "type": "string",
                        "enum": ["event", "finished"],
                        "description": "\"event\" (default): until something happens. \
                            \"finished\": until the command given by block finishes.",
                    },
                    "timeout_seconds": timeout("it"),
                },
            },
        },
    ]);
    if restart {
        let tools = tools.as_array_mut().expect("tools are an array");
        tools.push(start);
        tools.push(json!({
            "name": "close",
            "description": "Close hob, like closing its window: running commands are \
                interrupted, without asking. If it doesn't exit within 3 s, it's killed.",
            "inputSchema": {"type": "object", "properties": {}},
        }));
    }
    tools
}
