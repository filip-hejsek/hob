//! Runs the client's commands against MCP servers. The server is this test
//! program itself, started with `--server` (a server with some tools),
//! `--dies` (one that exits when its tools are listed), or `--fails` (one
//! that writes to stderr and exits right away).
//!
//! It has its own `main` (`harness = false`), so that it can act as the
//! server. Arguments that don't start with `-` filter the tests by name.

use std::panic;
use std::process::ExitCode;
use std::sync::Arc;

use hob_core::{Interpreter, Request};
use hob_mcp_test_client::Client;
use kettle_ui::Outcome;
use kettle_ui_headless::{Island, outline};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    Implementation, JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
    ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt};
use serde_json::json;
use tokio::runtime::Runtime;

const TESTS: &[(&str, fn())] = &[
    ("connects_and_lists_tools", connects_and_lists_tools),
    ("describes_a_tool", describes_a_tool),
    ("needs_a_connection", needs_a_connection),
    ("replaces_the_connection", replaces_the_connection),
    ("reports_missing_programs", reports_missing_programs),
    ("reports_servers_that_fail", reports_servers_that_fail),
    ("notices_servers_that_exit", notices_servers_that_exit),
    ("calls_with_json", calls_with_json),
    ("calls_without_arguments", calls_without_arguments),
    ("calls_through_a_form", calls_through_a_form),
    ("checks_the_form", checks_the_form),
    ("edits_arguments_as_json", edits_arguments_as_json),
    ("cancels_a_call", cancels_a_call),
    (
        "shows_images_and_structured_content",
        shows_images_and_structured_content,
    ),
    ("reports_tool_errors", reports_tool_errors),
    #[cfg(not(feature = "http"))]
    ("needs_http_for_urls", needs_http_for_urls),
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--server") => return serve(TestServer { dies: false }),
        Some("--dies") => return serve(TestServer { dies: true }),
        Some("--fails") => {
            eprintln!("bad config: no token");
            return ExitCode::FAILURE;
        }
        _ => {}
    }
    let filters: Vec<_> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let mut failed = 0;
    for (name, test) in TESTS {
        if !filters.is_empty() && !filters.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        let ok = panic::catch_unwind(test).is_ok();
        println!("test {name} ... {}", if ok { "ok" } else { "FAILED" });
        failed += usize::from(!ok);
    }
    if failed > 0 {
        println!("{failed} failed");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

struct TestServer {
    /// Exit when the tools are listed.
    dies: bool,
}

impl ServerHandler for TestServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("test-server", "1.2.3"))
            .with_instructions("Use echo to repeat things.")
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if self.dies {
            std::process::exit(0);
        }
        let schema = json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
        });
        let schema = schema.as_object().unwrap().clone();
        let mut echo = Tool::new("echo", "Repeat the text.\nAll of it.", schema.clone());
        echo.annotations = Some(ToolAnnotations::from_raw(
            None,
            Some(true),
            None,
            None,
            Some(false),
        ));
        let mut shout = Tool::new("shout", "Repeat the text, louder.", schema);
        shout.title = Some("Shout".to_owned());
        let form = object(json!({
            "type": "object",
            "properties": {
                "count": {"type": "integer", "description": "How many"},
                "loud": {"type": "boolean"},
                "mode": {"type": "string", "enum": ["fast", "slow"], "default": "fast"},
                "name": {"type": "string"},
                "ratio": {"type": ["number", "null"]},
                "tags": {"type": "array", "items": {"type": "string"}},
            },
            "required": ["name"],
        }));
        let form = Tool::new("form", "Return the arguments.", form);
        let empty = object(json!({"type": "object"}));
        let tools = vec![
            echo,
            Tool::new("fail", "Fail.", empty.clone()),
            form,
            Tool::new("picture", "Return a picture.", empty.clone()),
            Tool::new("ping", "Say pong.", empty),
            shout,
        ];
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let arguments = request.arguments.unwrap_or_default();
        let text = |name: &str| arguments.get(name).and_then(|v| v.as_str()).unwrap_or("");
        let result = match &*request.name {
            "echo" => CallToolResult::success(vec![ContentBlock::text(text("text"))]),
            "shout" => {
                CallToolResult::success(vec![ContentBlock::text(text("text").to_uppercase())])
            }
            "form" => {
                let arguments = serde_json::to_string(&arguments).unwrap();
                CallToolResult::success(vec![ContentBlock::text(arguments)])
            }
            "fail" => CallToolResult::error(vec![ContentBlock::text("It broke.")]),
            "ping" => CallToolResult::success(vec![ContentBlock::text("pong")]),
            "picture" => {
                let mut result =
                    CallToolResult::success(vec![ContentBlock::image(PIXEL, "image/png")]);
                result.structured_content = Some(json!({"width": 1}));
                result
            }
            _ => return Err(ErrorData::invalid_params("no such tool", None)),
        };
        Ok(result.into())
    }
}

/// A 1x1 PNG, base64-encoded.
const PIXEL: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

fn object(value: serde_json::Value) -> JsonObject {
    value.as_object().unwrap().clone()
}

/// Serves `server` on stdio, until the client closes it.
fn serve(server: TestServer) -> ExitCode {
    let runtime = Runtime::new().unwrap();
    runtime.block_on(async {
        let service = server.serve(rmcp::transport::stdio()).await.unwrap();
        let _ = service.waiting().await;
    });
    ExitCode::SUCCESS
}

/// The client's commands, and the runtime they use.
struct Harness {
    interpreter: Interpreter,
    client: Arc<Client>,
    _runtime: Runtime,
}

impl Harness {
    fn new() -> Self {
        let runtime = Runtime::new().unwrap();
        let client = Client::new(runtime.handle().clone());
        let mut interpreter = Interpreter::new();
        hob_mcp_test_client::register(&mut interpreter, &client);
        Harness {
            interpreter,
            client,
            _runtime: runtime,
        }
    }

    /// Runs `line` until it ends; returns what it prints or logs.
    fn run(&self, line: &str) -> Vec<String> {
        self.run_with(line, |_| panic!("unexpected island"))
    }

    /// Like [`Harness::run`], but `answer` acts on the islands shown (each
    /// is in the output, as it was shown).
    fn run_with(&self, line: &str, mut answer: impl FnMut(&mut Island) -> Outcome) -> Vec<String> {
        let requests = self.interpreter.run(line).unwrap();
        let requests = futures_executor::block_on_stream(requests);
        let output = requests.map(|request| match request {
            Request::Print(message) => message,
            Request::PrintIsland(island) => outline(&island),
            Request::Log(record) => record.to_string(),
            Request::Show(show) => {
                let (builder, reply) = show.into_parts();
                let mut island = Island::new(builder).unwrap();
                let shown = island.to_string();
                reply.send(answer(&mut island));
                shown
            }
            _ => panic!("unexpected request"),
        });
        output.collect()
    }

    /// Connects to this program, as a server started with `mode`.
    fn connect(&self, mode: &str) -> Vec<String> {
        let exe = std::env::current_exe().unwrap();
        self.run(&format!("connect '{}' {mode}", exe.display()))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.client.close();
    }
}

fn connects_and_lists_tools() {
    let h = Harness::new();
    let output = h.connect("--server");
    assert_eq!(output.len(), 2, "{output:?}");
    assert!(
        output[0].starts_with("Connected to test-server 1.2.3, using MCP "),
        "{output:?}"
    );
    assert!(output[0].ends_with(".\nIt offers: tools."), "{output:?}");
    assert_eq!(output[1], "Instructions:\nUse echo to repeat things.");
    assert_eq!(
        h.run("tools"),
        ["table (horizontal borders)\n\
          \x20 row\n\
          \x20   label \"echo\"\n\
          \x20   label \"Repeat the text.\"\n\
          \x20 row\n\
          \x20   label \"fail\"\n\
          \x20   label \"Fail.\"\n\
          \x20 row\n\
          \x20   label \"form\"\n\
          \x20   label \"Return the arguments.\"\n\
          \x20 row\n\
          \x20   label \"picture\"\n\
          \x20   label \"Return a picture.\"\n\
          \x20 row\n\
          \x20   label \"ping\"\n\
          \x20   label \"Say pong.\"\n\
          \x20 row\n\
          \x20   label \"shout\"\n\
          \x20   label \"Shout\"\n"]
    );
    assert_eq!(h.run("disconnect"), ["Disconnected from test-server."]);
}

fn describes_a_tool() {
    let h = Harness::new();
    h.connect("--server");
    assert_eq!(
        h.run("tools echo"),
        ["echo\n\
          \n\
          Repeat the text.\n\
          All of it.\n\
          \n\
          Hints: read-only: yes, open world: no\n\
          \n\
          Input schema:\n\
          {\n  \"properties\": {\n    \"text\": {\n      \"type\": \"string\"\n    }\n  },\n  \"type\": \"object\"\n}"]
    );
    assert!(h.run("tools shout")[0].starts_with("shout: Shout\n\nRepeat the text, louder.\n"));
    assert_eq!(h.run("tools nope"), ["error: no tool \"nope\""]);
}

fn needs_a_connection() {
    let h = Harness::new();
    assert_eq!(h.run("tools"), ["error: not connected"]);
    assert_eq!(h.run("disconnect"), ["error: not connected"]);
}

fn replaces_the_connection() {
    let h = Harness::new();
    h.connect("--server");
    let output = h.connect("--server");
    assert_eq!(output[0], "Disconnected from test-server.");
    assert!(
        output[1].starts_with("Connected to test-server"),
        "{output:?}"
    );
    assert_eq!(h.run("tools").len(), 1);
}

fn reports_missing_programs() {
    let h = Harness::new();
    let output = h.run("connect /nonexistent/server");
    assert_eq!(output.len(), 1, "{output:?}");
    assert!(
        output[0].starts_with("error: can't start /nonexistent/server: "),
        "{output:?}"
    );
}

fn reports_servers_that_fail() {
    let h = Harness::new();
    let output = h.connect("--fails");
    assert_eq!(output.len(), 2, "{output:?}");
    assert_eq!(output[0], "bad config: no token");
    assert!(
        output[1].starts_with("error: can't connect: "),
        "{output:?}"
    );
    assert_eq!(h.run("tools"), ["error: not connected"]);
}

fn notices_servers_that_exit() {
    let h = Harness::new();
    let output = h.connect("--dies");
    assert_eq!(
        output.last().unwrap(),
        "warning: can't list the tools: test-server has disconnected"
    );
    assert_eq!(h.run("tools"), ["error: not connected"]);
}

#[cfg(not(feature = "http"))]
fn needs_http_for_urls() {
    let h = Harness::new();
    assert_eq!(
        h.run("connect http://localhost:1/mcp"),
        ["error: built without HTTP support (the http feature)"]
    );
}

fn calls_with_json() {
    let h = Harness::new();
    h.connect("--server");
    assert_eq!(h.run(r#"call echo '{"text": "hi"}'"#), ["hi"]);
    // The arguments aren't checked: the server gets what was typed.
    assert_eq!(h.run("call form '{\"x\": 1}'"), [r#"{"x":1}"#]);
    assert_eq!(
        h.run("call echo '[1]'"),
        ["error: the arguments must be a JSON object"]
    );
}

fn calls_without_arguments() {
    let h = Harness::new();
    h.connect("--server");
    assert_eq!(h.run("call ping"), ["pong"]);
    assert_eq!(h.run("call nope"), ["error: no tool \"nope\""]);
}

fn calls_through_a_form() {
    let h = Harness::new();
    h.connect("--server");
    let output = h.run_with("call form", |island| {
        island.type_text("param0", "3").unwrap();
        island.set_checked("param1", true).unwrap();
        island.type_text("param3", "Ada").unwrap();
        island.type_text("param5", r#"["a", "b"]"#).unwrap();
        let preview = island.state("arguments").unwrap().to_string();
        assert_eq!(
            preview,
            r#"Arguments: {"count":3,"loud":true,"name":"Ada","tags":["a","b"]}"#
        );
        island.press("Call").unwrap()
    });
    assert_eq!(
        output,
        [
            "column\n\
             \x20 label \"Call form\"\n\
             \x20 column\n\
             \x20   label \"count (integer)\"\n\
             \x20   label \"How many\"\n\
             \x20   text_field param0: \"\"\n\
             \x20 column\n\
             \x20   checkbox param1: [ ] \"loud (boolean)\"\n\
             \x20 column\n\
             \x20   label \"mode (string)\"\n\
             \x20   text_field param2: \"\" (placeholder \"one of: fast, slow; default: fast\")\n\
             \x20 column\n\
             \x20   label \"name (string, required)\"\n\
             \x20   text_field param3: \"\"\n\
             \x20 column\n\
             \x20   label \"ratio (number or null)\"\n\
             \x20   text_field param4: \"\"\n\
             \x20 column\n\
             \x20   label \"tags (array, as JSON)\"\n\
             \x20   text_area param5: \"\"\n\
             \x20 label \"Arguments: {}\"\n\
             \x20 row\n\
             \x20   button \"Call\"\n\
             \x20   button \"Edit as JSON\"\n\
             \x20   button \"Cancel\"\n",
            r#"{"count":3,"loud":true,"name":"Ada","tags":["a","b"]}"#,
        ]
    );
}

fn checks_the_form() {
    let h = Harness::new();
    h.connect("--server");
    let mut shown = 0;
    let output = h.run_with("call form", |island| {
        shown += 1;
        if shown == 1 {
            island.type_text("param0", "2.5").unwrap();
            island.type_text("param4", "x").unwrap();
            island.type_text("param5", "[").unwrap();
            let problems = island.state("arguments").unwrap().to_string();
            assert!(
                problems.starts_with(
                    "count: not an integer\nratio: not a number\ntags: invalid JSON: "
                ),
                "{problems}"
            );
        } else {
            // Shown again as it was left.
            assert_eq!(island.fields().text("param0"), "2.5");
            island.type_text("param0", "").unwrap();
            island.type_text("param4", "0.5").unwrap();
            island.type_text("param5", "").unwrap();
        }
        island.press("Call").unwrap()
    });
    assert_eq!(shown, 2);
    assert!(
        output[1].starts_with("error: count: not an integer\n"),
        "{output:?}"
    );
    assert_eq!(output.last().unwrap(), r#"{"ratio":0.5}"#);
}

fn edits_arguments_as_json() {
    let h = Harness::new();
    h.connect("--server");
    let mut shown = 0;
    let output = h.run_with("call form", |island| {
        shown += 1;
        match shown {
            1 => {
                island.type_text("param3", "Ada").unwrap();
                island.press("Edit as JSON").unwrap()
            }
            2 => {
                assert_eq!(island.fields().text("json"), "{\n  \"name\": \"Ada\"\n}");
                island.type_text("json", "{").unwrap();
                let problem = island.state("problem").unwrap().to_string();
                assert!(problem.starts_with("invalid JSON: "), "{problem}");
                island.press("Call").unwrap()
            }
            _ => {
                island.type_text("json", r#"{"name": 5}"#).unwrap();
                island.press("Call").unwrap()
            }
        }
    });
    assert_eq!(shown, 3);
    assert!(output[2].starts_with("error: invalid JSON: "), "{output:?}");
    assert_eq!(output.last().unwrap(), r#"{"name":5}"#);
}

fn cancels_a_call() {
    let h = Harness::new();
    h.connect("--server");
    let output = h.run_with("call echo", |island| island.press("Cancel").unwrap());
    assert_eq!(output[1], "Not called.");
}

fn shows_images_and_structured_content() {
    let h = Harness::new();
    h.connect("--server");
    assert_eq!(
        h.run("call picture"),
        [
            "image \"image/png image from picture\" (1x1)\n",
            "Structured content:\n{\n  \"width\": 1\n}",
        ]
    );
}

fn reports_tool_errors() {
    let h = Harness::new();
    h.connect("--server");
    assert_eq!(
        h.run("call fail"),
        ["It broke.", "error: the tool reported an error"]
    );
}
