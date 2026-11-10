//! Runs the client's commands against MCP servers. The server is this test
//! program itself, started with `--server` (a server with some tools),
//! `--dies` (one that exits when its tools are listed), or `--fails` (one
//! that writes to stderr and exits right away).
//!
//! It has its own `main` (`harness = false`), so that it can act as the
//! server. Arguments that don't start with `-` filter the tests by name.
//!
//! The server also has tools it doesn't list, for notifications: `work`
//! sends progress and log messages before its result, and `change` sends
//! notifications after it.

#![expect(deprecated, reason = "MCP logging is used before 2026-07-28")]

use std::panic;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hob_core::{Interpreter, Level, Request};
use hob_mcp_test_client::Client;
use kettle_ui::Outcome;
use kettle_ui_headless::{Island, outline};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, EmbeddedResource,
    ErrorData, GetPromptRequestParams, GetPromptResponse, GetPromptResult, Implementation,
    JsonObject, ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult,
    ListToolsResult, LoggingLevel, LoggingMessageNotificationParam, PaginatedRequestParams,
    ProgressNotificationParam, Prompt, PromptArgument, PromptMessage, ReadResourceRequestParams,
    ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, ResourceTemplate,
    ResourceUpdatedNotificationParam, Role, ServerCapabilities, ServerConfig,
    SetLevelRequestParams, SubscribeRequestParams, Tool, ToolAnnotations, UnsubscribeRequestParams,
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
    ("reconnects", reconnects),
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
    ("lists_resources", lists_resources),
    ("reads_resources", reads_resources),
    ("lists_and_describes_prompts", lists_and_describes_prompts),
    ("gets_prompts_through_a_form", gets_prompts_through_a_form),
    ("gets_prompts_with_arguments", gets_prompts_with_arguments),
    (
        "shows_notifications_in_the_block",
        shows_notifications_in_the_block,
    ),
    ("logs_other_notifications", logs_other_notifications),
    ("subscribes_to_resources", subscribes_to_resources),
    ("sets_the_log_level", sets_the_log_level),
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
    // What's logged outside commands, as commands' log records go to them.
    let records = hob_core::logger::install(Level::Info, "test".to_owned()).unwrap();
    std::thread::spawn(move || {
        for record in futures_executor::block_on_stream(records) {
            LOGGED.lock().unwrap().push(record.to_string());
        }
    });
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

/// The log records from outside commands, as text.
static LOGGED: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct TestServer {
    /// Exit when the tools are listed.
    dies: bool,
}

impl ServerHandler for TestServer {
    fn get_info(&self) -> ServerConfig {
        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_resources_subscribe()
            .enable_prompts()
            .enable_logging()
            .build();
        ServerConfig::new(capabilities)
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
        context: RequestContext<RoleServer>,
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
            "work" => {
                let peer = &context.peer;
                if let Some(token) = context.meta.get_progress_token() {
                    let mut progress = ProgressNotificationParam::new(token, 1.0);
                    progress.total = Some(2.0);
                    progress.message = Some("Halfway".to_owned());
                    peer.notify_progress(progress).await.unwrap();
                }
                let mut message =
                    LoggingMessageNotificationParam::new(LoggingLevel::Warning, json!("careful"));
                message.logger = Some("work".to_owned());
                peer.notify_logging_message(message).await.unwrap();
                let message =
                    LoggingMessageNotificationParam::new(LoggingLevel::Info, json!({"step": 2}));
                peer.notify_logging_message(message).await.unwrap();
                // So the notifications are handled before the result.
                tokio::time::sleep(Duration::from_millis(50)).await;
                CallToolResult::success(vec![ContentBlock::text("Done.")])
            }
            "change" => {
                let peer = context.peer.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    peer.notify_tool_list_changed().await.unwrap();
                    let message =
                        LoggingMessageNotificationParam::new(LoggingLevel::Error, json!("late"));
                    peer.notify_logging_message(message).await.unwrap();
                });
                CallToolResult::success(vec![ContentBlock::text("Changing.")])
            }
            _ => return Err(ErrorData::invalid_params("no such tool", None)),
        };
        Ok(result.into())
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let peer = context.peer.clone();
        tokio::spawn(async move {
            for _ in 0..2 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let updated = ResourceUpdatedNotificationParam::new(request.uri.clone());
                peer.notify_resource_updated(updated).await.unwrap();
            }
        });
        Ok(())
    }

    async fn unsubscribe(
        &self,
        _: UnsubscribeRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        Ok(())
    }

    async fn set_level(
        &self,
        _: SetLevelRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        Ok(())
    }

    async fn list_resources(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let resources = vec![
            Resource::new("test://hello", "hello.txt").with_title("Greeting"),
            Resource::new("test://pixel", "pixel.png"),
            Resource::new("test://both", "both"),
        ];
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn list_resource_templates(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        let template = ResourceTemplate::new("test://items/{id}", "item");
        Ok(ListResourceTemplatesResult::with_all_items(vec![template]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let contents = match request.uri.as_str() {
            "test://hello" => vec![hello()],
            "test://pixel" => {
                vec![ResourceContents::blob(PIXEL, "test://pixel").with_mime_type("image/png")]
            }
            "test://both" => vec![
                ResourceContents::text("A", "test://a"),
                ResourceContents::blob("AAAA", "test://b"),
            ],
            _ => return Err(ErrorData::resource_not_found("no such resource", None)),
        };
        Ok(ReadResourceResult::new(contents).into())
    }

    async fn list_prompts(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        let arguments = vec![
            PromptArgument::new("name")
                .with_description("Who to greet")
                .with_required(true),
            PromptArgument::new("mood").with_title("Mood"),
        ];
        let prompts = vec![
            Prompt::new("greet", Some("Greet someone.\nNicely."), Some(arguments)),
            Prompt::new("plain", None::<String>, None),
        ];
        Ok(ListPromptsResult::with_all_items(prompts))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let arguments = request.arguments.unwrap_or_default();
        let text = |name: &str| arguments.get(name).and_then(|v| v.as_str()).unwrap_or("");
        let result = match request.name.as_str() {
            "greet" => GetPromptResult::new(vec![
                PromptMessage::new_text(Role::User, format!("Greet {}.", text("name"))),
                PromptMessage::new_text(Role::Assistant, format!("Hello{}!", text("mood"))),
            ])
            .with_description("A greeting"),
            "plain" => GetPromptResult::new(vec![
                PromptMessage::new(Role::User, ContentBlock::image(PIXEL, "image/png")),
                PromptMessage::new(
                    Role::Assistant,
                    ContentBlock::Resource(EmbeddedResource::new(hello())),
                ),
            ]),
            _ => return Err(ErrorData::invalid_params("no such prompt", None)),
        };
        Ok(result.into())
    }
}

/// A 1x1 PNG, base64-encoded.
const PIXEL: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

fn object(value: serde_json::Value) -> JsonObject {
    value.as_object().unwrap().clone()
}

fn hello() -> ResourceContents {
    ResourceContents::text("Hello!", "test://hello")
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
    assert!(
        output[0].ends_with(".\nIt offers: tools, resources, prompts, logging."),
        "{output:?}"
    );
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

fn reconnects() {
    let h = Harness::new();
    assert_eq!(h.run("reconnect"), ["error: no server to reconnect to"]);
    h.connect("--server");
    let output = h.run("reconnect");
    assert_eq!(output[0], "Disconnected from test-server.");
    assert!(
        output[1].starts_with("Connected to test-server"),
        "{output:?}"
    );
    // Also after the connection has been closed.
    h.run("disconnect");
    let output = h.run("reconnect");
    assert!(
        output[0].starts_with("Connected to test-server"),
        "{output:?}"
    );
    // A failed attempt is the server to reconnect to.
    h.run("connect /nonexistent/server");
    let output = h.run("reconnect");
    assert_eq!(output[0], "Disconnected from test-server.");
    assert!(
        output[1].starts_with("error: can't start /nonexistent/server: "),
        "{output:?}"
    );
    assert_eq!(h.run("tools"), ["error: not connected"]);
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
             \x20   button \"Call\" (submit)\n\
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

fn lists_resources() {
    let h = Harness::new();
    h.connect("--server");
    assert_eq!(
        h.run("resources"),
        ["column\n\
          \x20 label \"Resources:\"\n\
          \x20 table (horizontal borders)\n\
          \x20   row\n\
          \x20     label \"test://hello\"\n\
          \x20     label \"Greeting\"\n\
          \x20   row\n\
          \x20     label \"test://pixel\"\n\
          \x20     label \"pixel.png\"\n\
          \x20   row\n\
          \x20     label \"test://both\"\n\
          \x20     label \"both\"\n\
          \x20 label \"Templates:\"\n\
          \x20 table (horizontal borders)\n\
          \x20   row\n\
          \x20     label \"test://items/{id}\"\n\
          \x20     label \"item\"\n"]
    );
}

fn reads_resources() {
    let h = Harness::new();
    h.connect("--server");
    assert_eq!(h.run("read test://hello"), ["Hello!"]);
    assert_eq!(
        h.run("read test://pixel"),
        ["image \"image/png image test://pixel\" (1x1)\n"]
    );
    assert_eq!(
        h.run("read test://both"),
        ["test://a:\nA", "test://b:\nbinary data, 3 bytes"]
    );
    let output = h.run("read test://nope");
    assert_eq!(output.len(), 1, "{output:?}");
    assert!(output[0].starts_with("error: "), "{output:?}");
    let completion = h.interpreter.complete("read test://h");
    assert_eq!(completion.line, "read test://hello ");
    let completion = h.interpreter.complete("prompts g");
    assert_eq!(completion.line, "prompts greet ");
}

fn lists_and_describes_prompts() {
    let h = Harness::new();
    h.connect("--server");
    assert_eq!(
        h.run("prompts"),
        ["table (horizontal borders)\n\
          \x20 row\n\
          \x20   label \"greet\"\n\
          \x20   label \"Greet someone.\"\n\
          \x20 row\n\
          \x20   label \"plain\"\n\
          \x20   label \"\"\n"]
    );
    assert_eq!(
        h.run("prompts greet"),
        ["greet\n\
          \n\
          Greet someone.\n\
          Nicely.\n\
          \n\
          Arguments:\n\
          - name (required): Who to greet\n\
          - mood: Mood"]
    );
    assert_eq!(h.run("prompts plain"), ["plain\n\nArguments: none"]);
    assert_eq!(h.run("prompts nope"), ["error: no prompt \"nope\""]);
}

fn gets_prompts_through_a_form() {
    let h = Harness::new();
    h.connect("--server");
    let output = h.run_with("prompt greet", |island| {
        island.type_text("arg0", "Ada").unwrap();
        island.press("Get").unwrap()
    });
    assert_eq!(
        output,
        [
            "column\n\
             \x20 label \"Get prompt greet\"\n\
             \x20 column\n\
             \x20   label \"name (required)\"\n\
             \x20   label \"Who to greet\"\n\
             \x20   text_field arg0: \"\"\n\
             \x20 column\n\
             \x20   label \"mood: Mood\"\n\
             \x20   text_field arg1: \"\"\n\
             \x20 row\n\
             \x20   button \"Get\" (submit)\n\
             \x20   button \"Cancel\"\n",
            "Description: A greeting",
            "User:\nGreet Ada.",
            "Assistant:\nHello!",
        ]
    );
    let output = h.run_with("prompt greet", |island| island.press("Cancel").unwrap());
    assert_eq!(output[1], "Cancelled.");
}

fn gets_prompts_with_arguments() {
    let h = Harness::new();
    h.connect("--server");
    assert_eq!(
        h.run("prompt greet name=Ada 'mood=, dear'"),
        [
            "Description: A greeting",
            "User:\nGreet Ada.",
            "Assistant:\nHello, dear!",
        ]
    );
    assert_eq!(
        h.run("prompt greet Ada"),
        ["error: expected NAME=VALUE, not \"Ada\""]
    );
    // Without arguments: not asked for any.
    assert_eq!(
        h.run("prompt plain"),
        [
            "User:",
            "image \"image/png image from plain\" (1x1)\n",
            "Assistant:\nResource test://hello:\nHello!",
        ]
    );
}

fn shows_notifications_in_the_block() {
    let h = Harness::new();
    h.connect("--server");
    assert_eq!(
        h.run("call work '{}'"),
        [
            "Progress: 1 of 2: Halfway",
            "warning: work: careful",
            "info: {\"step\":2}",
            "Done.",
        ]
    );
}

fn logs_other_notifications() {
    let h = Harness::new();
    h.connect("--server");
    assert_eq!(h.run("call change '{}'"), ["Changing."]);
    let expected = ["info: The server's tools have changed.", "error: late"];
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let logged = LOGGED.lock().unwrap().clone();
        if expected.iter().all(|e| logged.iter().any(|l| l == e)) {
            break;
        }
        assert!(Instant::now() < deadline, "{logged:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn subscribes_to_resources() {
    let h = Harness::new();
    h.connect("--server");
    let output = h.run_with("subscribe test://hello", |island| {
        let updated = island.wait_until(Duration::from_secs(5), |island| {
            matches!(island.state("updates"), Some(kettle_ui::Value::List(l)) if l.len() == 2)
        });
        assert!(updated, "{island}");
        island.press("Unsubscribe").unwrap()
    });
    assert_eq!(
        output,
        [
            "column\n\
             \x20 label \"Subscribed to test://hello\"\n\
             \x20 list\n\
             \x20 button \"Unsubscribe\"\n",
            "Unsubscribed from test://hello.",
        ]
    );
}

fn sets_the_log_level() {
    let h = Harness::new();
    h.connect("--server");
    assert_eq!(
        h.run("log-level warning"),
        ["The server now sends log messages from warning up."]
    );
    let output = h.run("log-level loud");
    assert!(
        output[0].starts_with("error: no level \"loud\""),
        "{output:?}"
    );
}
