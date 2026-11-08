//! Runs the client's commands against MCP servers. The server is this test
//! program itself, started with `--server` (a server with one tool),
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
use kettle_ui_headless::outline;
use rmcp::model::{
    ErrorData, Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
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
        Ok(ListToolsResult::with_all_items(vec![echo, shout]))
    }
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
        let requests = self.interpreter.run(line).unwrap();
        let requests = futures_executor::block_on_stream(requests);
        let output = requests.map(|request| match request {
            Request::Print(message) => message,
            Request::PrintIsland(island) => outline(&island),
            Request::Log(record) => record.to_string(),
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
