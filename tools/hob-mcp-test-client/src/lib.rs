//! An MCP client for people: commands that connect to an MCP server and use
//! it the way an agent would. [`register`] adds them to a hob interpreter;
//! the `hob-mcp-test-client` program shows them in hob's GUI.
//!
//! - `connect SERVER [ARG...]`: starts the program SERVER with ARGs, in the
//!   working directory, and connects to it over stdio. With the `http`
//!   feature, SERVER can instead be an `http://` or `https://` URL. A new
//!   connection replaces the old one.
//! - `disconnect`: closes the connection.
//! - `tools [TOOL]`: lists the server's tools, or describes one.
//!
//! What a server writes to stderr is logged (info, so `messages` shows it).
//! If it fails to connect, its stderr so far is printed too.

use std::error::Error;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use hob_core::args::{Kind, Matches, Spec};
use hob_core::{CommandResult, Context, Interpreter};
use kettle_ui::IslandBuilder;
use kettle_ui::widget::{label, table_horizontal};
use rmcp::model::{
    ClientCapabilities, ClientConfig, Implementation, ServerCapabilities, ServerPeerInfo, Tool,
};
use rmcp::service::{Peer, RunningService, ServiceError};
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::ChildStderr;
use tokio::runtime::Handle;

/// The log target of what servers write to stderr.
const STDERR_TARGET: &str = "hob_mcp_test_client::server";

/// How many lines of a server's stderr are printed when it fails to connect
/// (the last ones).
const STDERR_LINES: usize = 50;

/// How long closing a connection may take; then it's dropped (and a server
/// process killed).
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(feature = "http")]
const CONNECT_DESCRIPTION: &str =
    "Connect to an MCP server: start a program with ARGs, or open an http(s) URL";
#[cfg(not(feature = "http"))]
const CONNECT_DESCRIPTION: &str = "Start an MCP server (a program with ARGs) and connect to it";

type BoxError = Box<dyn Error + Send + Sync>;

/// The client's state, shared by its commands: the connection to a server,
/// if any.
pub struct Client {
    runtime: Handle,
    connection: Mutex<Option<Connection>>,
    /// The names of the server's tools, as last listed, for completion.
    tool_names: Arc<Mutex<Vec<String>>>,
    next_id: AtomicU64,
}

struct Connection {
    service: RunningService<RoleClient, ClientConfig>,
    /// Tells connections apart, so a command that finds its connection
    /// closed doesn't forget a newer one.
    id: u64,
    /// What to call the server: its name, or what the user connected to.
    name: String,
}

impl Client {
    /// A client without a connection. Its commands block on `runtime` to
    /// talk to the server.
    pub fn new(runtime: Handle) -> Arc<Self> {
        Arc::new(Client {
            runtime,
            connection: Mutex::new(None),
            tool_names: Arc::new(Mutex::new(Vec::new())),
            next_id: AtomicU64::new(0),
        })
    }

    /// Closes the connection, if any, waiting for it (a few seconds at most).
    /// Call it before the program exits, so a server process is ended.
    pub fn close(&self) {
        if let Some(connection) = lock(&self.connection).take() {
            self.close_connection(connection);
        }
    }

    fn close_connection(&self, mut connection: Connection) {
        lock(&self.tool_names).clear();
        let close = connection.service.close_with_timeout(CLOSE_TIMEOUT);
        let _ = self.runtime.block_on(close);
    }

    fn connect(&self, ctx: &mut Context, args: &Matches) -> CommandResult {
        let server = args.value("SERVER").ok_or("no server")?;
        let rest = args.values("ARG");
        let service = if is_url(server) {
            self.open_url(server, rest)?
        } else {
            self.start(ctx, server, rest)?
        };
        let info = service.peer_info();
        let server_name = (info.as_ref())
            .and_then(|info| info.server_info.as_ref())
            .map(|implementation| implementation.name.clone());
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let name = server_name.unwrap_or_else(|| server.to_owned());
        let connection = Connection { service, id, name };
        let peer = connection.service.peer().clone();
        let old = lock(&self.connection).replace(connection);
        if let Some(old) = old {
            ctx.print(format!("Disconnected from {}.", old.name));
            self.close_connection(old);
        }
        let Some(info) = info else {
            ctx.print(format!("Connected to {server}."));
            return Ok(());
        };
        ctx.print(describe_server(&info));
        if let Some(instructions) = &info.instructions {
            ctx.print(format!("Instructions:\n{instructions}"));
        }
        if info.capabilities.tools.is_some() {
            // For completion.
            match self.request(id, peer.list_all_tools()) {
                Ok(tools) => self.remember_tools(&tools),
                Err(e) => ctx.warn(format!("can't list the tools: {e}")),
            }
        }
        Ok(())
    }

    /// Starts `program` with `args`, and connects to it over stdio.
    fn start(
        &self,
        ctx: &mut Context,
        program: &str,
        args: &[String],
    ) -> Result<RunningService<RoleClient, ClientConfig>, BoxError> {
        let mut command = tokio::process::Command::new(program);
        command.args(args).current_dir(ctx.cwd());
        // Lines of stderr, kept while connecting.
        let captured = Arc::new(Mutex::new(Some(Vec::new())));
        let result = self.runtime.block_on(async {
            let builder = TokioChildProcess::builder(command).stderr(Stdio::piped());
            let (transport, stderr) = builder
                .spawn()
                .map_err(|e| format!("can't start {program}: {e}"))?;
            let reader = stderr.map(|stderr| tokio::spawn(forward(stderr, captured.clone())));
            match client_config().serve(transport).await {
                Ok(service) => Ok(service),
                Err(e) => {
                    // The server has likely exited: let its stderr end.
                    if let Some(reader) = reader {
                        let _ = tokio::time::timeout(Duration::from_secs(1), reader).await;
                    }
                    Err(format!("can't connect: {e}"))
                }
            }
        });
        let stderr = lock(&captured).take().unwrap_or_default();
        if result.is_err() && !stderr.is_empty() {
            ctx.print(stderr.join("\n"));
        }
        Ok(result?)
    }

    #[cfg(feature = "http")]
    fn open_url(
        &self,
        url: &str,
        args: &[String],
    ) -> Result<RunningService<RoleClient, ClientConfig>, BoxError> {
        use rmcp::transport::StreamableHttpClientTransport;
        if !args.is_empty() {
            return Err("a URL takes no arguments".into());
        }
        let service = self.runtime.block_on(async {
            let transport = StreamableHttpClientTransport::from_uri(url);
            client_config().serve(transport).await
        });
        Ok(service.map_err(|e| format!("can't connect: {e}"))?)
    }

    #[cfg(not(feature = "http"))]
    fn open_url(
        &self,
        _: &str,
        _: &[String],
    ) -> Result<RunningService<RoleClient, ClientConfig>, BoxError> {
        Err("built without HTTP support (the http feature)".into())
    }

    fn disconnect(&self, ctx: &mut Context, _: &Matches) -> CommandResult {
        let connection = lock(&self.connection).take().ok_or("not connected")?;
        let name = connection.name.clone();
        self.close_connection(connection);
        ctx.print(format!("Disconnected from {name}."));
        Ok(())
    }

    fn tools(&self, ctx: &mut Context, args: &Matches) -> CommandResult {
        let (peer, id) = self.peer()?;
        let offers_tools = (peer.peer_info()).is_some_and(|info| info.capabilities.tools.is_some());
        if !offers_tools {
            return Err("the server doesn't offer tools".into());
        }
        let tools = self.request(id, peer.list_all_tools())?;
        self.remember_tools(&tools);
        match args.value("TOOL") {
            None if tools.is_empty() => ctx.print("The server has no tools."),
            None => {
                let rows = (tools.iter()).map(|tool| [label(&*tool.name), label(summary(tool))]);
                let table = table_horizontal(rows.collect::<Vec<_>>());
                ctx.print_island(IslandBuilder::new(table).freeze()?);
            }
            Some(name) => {
                let tool = (tools.iter().find(|tool| tool.name == name))
                    .ok_or_else(|| format!("no tool {name:?}"))?;
                ctx.print(describe_tool(tool));
            }
        }
        Ok(())
    }

    fn remember_tools(&self, tools: &[Tool]) {
        *lock(&self.tool_names) = tools.iter().map(|tool| tool.name.to_string()).collect();
    }

    /// The connection's peer, to send requests to, and the connection's id.
    fn peer(&self) -> Result<(Peer<RoleClient>, u64), BoxError> {
        let connection = lock(&self.connection);
        let connection = connection.as_ref().ok_or("not connected")?;
        Ok((connection.service.peer().clone(), connection.id))
    }

    /// Waits for a request to connection `id`. If the connection turns out to
    /// be closed (e.g. the server exited), it's forgotten.
    fn request<T>(
        &self,
        id: u64,
        request: impl Future<Output = Result<T, ServiceError>>,
    ) -> Result<T, BoxError> {
        match self.runtime.block_on(request) {
            Ok(result) => Ok(result),
            Err(ServiceError::TransportClosed) => {
                let mut connection = lock(&self.connection);
                if connection.as_ref().is_some_and(|c| c.id == id) {
                    let name = connection.take().map(|c| c.name).unwrap_or_default();
                    lock(&self.tool_names).clear();
                    return Err(format!("{name} has disconnected").into());
                }
                Err(ServiceError::TransportClosed.into())
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// Registers the client's commands, which share `client`.
pub fn register(interpreter: &mut Interpreter, client: &Arc<Client>) {
    let spec = Spec::new()
        .arg("SERVER", Kind::Path)
        .arg("ARG", Kind::Text)
        .optional()
        .repeated();
    let c = client.clone();
    interpreter.register_spec("connect", CONNECT_DESCRIPTION, spec, move |ctx, args| {
        c.connect(ctx, args)
    });
    let c = client.clone();
    interpreter.register_spec(
        "disconnect",
        "Close the connection to the server",
        Spec::new(),
        move |ctx, args| c.disconnect(ctx, args),
    );
    let names = client.tool_names.clone();
    let tool = Kind::custom(move |_| lock(&names).clone());
    let c = client.clone();
    interpreter.register_spec(
        "tools",
        "List the server's tools, or describe one",
        Spec::new().arg("TOOL", tool).optional(),
        move |ctx, args| c.tools(ctx, args),
    );
}

/// How the client introduces itself to servers.
fn client_config() -> ClientConfig {
    let implementation = Implementation::new("hob-mcp-test-client", env!("CARGO_PKG_VERSION"));
    ClientConfig::new(ClientCapabilities::default(), implementation)
}

fn is_url(server: &str) -> bool {
    server.starts_with("http://") || server.starts_with("https://")
}

/// Logs the lines of a server's stderr, and keeps them in `captured` while
/// it's `Some`, up to the last [`STDERR_LINES`].
async fn forward(stderr: ChildStderr, captured: Arc<Mutex<Option<Vec<String>>>>) {
    let mut stderr = BufReader::new(stderr);
    let mut line = Vec::new();
    // Not `lines()`: it stops at invalid UTF-8, and an unread pipe would
    // block the server.
    while let Ok(1..) = stderr.read_until(b'\n', &mut line).await {
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\n', '\r']).to_owned();
        line.clear();
        log::info!(target: STDERR_TARGET, "{text}");
        if let Some(captured) = lock(&captured).as_mut() {
            if captured.len() == STDERR_LINES {
                captured.remove(0);
            }
            captured.push(text);
        }
    }
}

/// The server's name and version, the protocol version, and what it offers.
fn describe_server(info: &ServerPeerInfo) -> String {
    let server = match &info.server_info {
        Some(implementation) => {
            let mut server = format!("{} {}", implementation.name, implementation.version);
            if let Some(title) = &implementation.title {
                server = format!("{title} ({server})");
            }
            server
        }
        None => "the server".to_owned(),
    };
    let offers = offers(&info.capabilities);
    let offers = match offers.is_empty() {
        true => "nothing".to_owned(),
        false => offers.join(", "),
    };
    format!(
        "Connected to {server}, using MCP {}.\nIt offers: {offers}.",
        info.protocol_version
    )
}

/// What the server offers, by capability.
fn offers(capabilities: &ServerCapabilities) -> Vec<&'static str> {
    let offered = [
        ("tools", capabilities.tools.is_some()),
        ("resources", capabilities.resources.is_some()),
        ("prompts", capabilities.prompts.is_some()),
        ("completions", capabilities.completions.is_some()),
        ("logging", capabilities.logging.is_some()),
    ];
    (offered.into_iter())
        .filter_map(|(name, offered)| offered.then_some(name))
        .collect()
}

/// A tool in one line: its title, or the first line of its description.
fn summary(tool: &Tool) -> String {
    let description = tool.description.as_deref().and_then(|d| d.lines().next());
    (tool.title.as_deref().or(description))
        .unwrap_or_default()
        .to_owned()
}

/// All about a tool: title, description, hints and schemas.
fn describe_tool(tool: &Tool) -> String {
    let mut sections = Vec::new();
    sections.push(match &tool.title {
        Some(title) => format!("{}: {title}", tool.name),
        None => tool.name.to_string(),
    });
    if let Some(description) = &tool.description {
        sections.push(description.to_string());
    }
    if let Some(annotations) = &tool.annotations {
        let hints = [
            ("read-only", annotations.read_only_hint),
            ("destructive", annotations.destructive_hint),
            ("idempotent", annotations.idempotent_hint),
            ("open world", annotations.open_world_hint),
        ];
        let hints: Vec<String> = (hints.into_iter())
            .filter_map(|(name, hint)| Some(format!("{name}: {}", yes_no(hint?))))
            .collect();
        if !hints.is_empty() {
            sections.push(format!("Hints: {}", hints.join(", ")));
        }
    }
    sections.push(format!("Input schema:\n{}", pretty(&tool.input_schema)));
    if let Some(schema) = &tool.output_schema {
        sections.push(format!("Output schema:\n{}", pretty(schema)));
    }
    sections.join("\n\n")
}

fn yes_no(hint: bool) -> &'static str {
    if hint { "yes" } else { "no" }
}

fn pretty(json: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(json).unwrap_or_else(|e| format!("({e})"))
}

/// Locks `mutex`, also if a thread panicked while holding it: the state
/// stays usable.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
