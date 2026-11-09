//! An MCP client for people: commands that connect to an MCP server and use
//! it the way an agent would. [`register`] adds them to a hob interpreter;
//! the `hob-mcp-test-client` program shows them in hob's GUI.
//!
//! - `connect SERVER [ARG...]`: starts the program SERVER with ARGs, in the
//!   working directory, and connects to it over stdio. With the `http`
//!   feature, SERVER can instead be an `http://` or `https://` URL. A new
//!   connection replaces the old one.
//! - `disconnect`: closes the connection.
//! - `reconnect`: closes the connection, if any, and connects again to the
//!   server last connected to (or tried), in the same directory.
//! - `tools [TOOL]`: lists the server's tools, or describes one.
//! - `call TOOL [JSON]`: calls a tool, with arguments filled in through a
//!   form built from its input schema, or given as a JSON object.
//! - `resources`: lists the server's resources and resource templates.
//! - `read URI`: reads a resource.
//! - `prompts [PROMPT]`: lists the server's prompts, or describes one.
//! - `prompt PROMPT [NAME=VALUE...]`: gets a prompt, with arguments filled
//!   in through a form, or given as NAME=VALUE.
//!
//! What a server writes to stderr is logged (info, so `messages` shows it).
//! If it fails to connect, its stderr so far is printed too.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

mod call;
mod content;
mod prompt;

use hob_core::args::{Kind, Matches, Spec};
use hob_core::{CommandResult, Context, Interpreter};
use kettle_ui::IslandBuilder;
use kettle_ui::widget::{column, label, table_horizontal};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ClientCapabilities, ClientConfig,
    GetPromptRequestParams, GetPromptResponse, Implementation, Prompt, ReadResourceRequestParams,
    ReadResourceResponse, Resource, ServerCapabilities, ServerPeerInfo, Tool,
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
    /// The server last connected to (or tried), for `reconnect`.
    last_server: Mutex<Option<Server>>,
    /// What the server offers, as last listed, for completion.
    names: Arc<Mutex<Names>>,
    next_id: AtomicU64,
}

/// A server to connect to, as `connect` was given it.
#[derive(Clone)]
struct Server {
    /// A program, or a URL.
    server: String,
    args: Vec<String>,
    /// The working directory, for a program.
    cwd: PathBuf,
}

/// The names of what a server offers, for completion.
#[derive(Default)]
struct Names {
    tools: Vec<String>,
    /// The resources' URIs.
    resources: Vec<String>,
    prompts: Vec<String>,
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
            last_server: Mutex::new(None),
            names: Arc::default(),
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
        *lock(&self.names) = Names::default();
        let close = connection.service.close_with_timeout(CLOSE_TIMEOUT);
        let _ = self.runtime.block_on(close);
    }

    fn connect(&self, ctx: &mut Context, args: &Matches) -> CommandResult {
        let server = Server {
            server: args.value("SERVER").ok_or("no server")?.to_owned(),
            args: args.values("ARG").to_vec(),
            cwd: ctx.cwd().to_owned(),
        };
        *lock(&self.last_server) = Some(server.clone());
        self.connect_to(ctx, &server)
    }

    fn reconnect(&self, ctx: &mut Context, _: &Matches) -> CommandResult {
        let server = lock(&self.last_server).clone();
        let server = server.ok_or("no server to reconnect to")?;
        let old = lock(&self.connection).take();
        if let Some(old) = old {
            ctx.print(format!("Disconnected from {}.", old.name));
            self.close_connection(old);
        }
        self.connect_to(ctx, &server)
    }

    /// Connects to `server`, replacing the connection once it succeeds.
    fn connect_to(&self, ctx: &mut Context, server: &Server) -> CommandResult {
        let Server { server, args, cwd } = server;
        let service = if is_url(server) {
            self.open_url(server, args)?
        } else {
            self.start(ctx, server, args, cwd)?
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
        // For completion. Stops at the first failure: the server may be gone.
        let capabilities = &info.capabilities;
        let listed = (|| {
            if capabilities.tools.is_some() {
                self.list_tools(&peer, id).map_err(|e| ("tools", e))?;
            }
            if capabilities.resources.is_some() {
                self.list_resources(&peer, id)
                    .map_err(|e| ("resources", e))?;
            }
            if capabilities.prompts.is_some() {
                self.list_prompts(&peer, id).map_err(|e| ("prompts", e))?;
            }
            Ok(())
        })();
        if let Err((what, e)) = listed {
            ctx.warn(format!("can't list the {what}: {e}"));
        }
        Ok(())
    }

    /// Starts `program` with `args` in `cwd`, and connects to it over stdio.
    fn start(
        &self,
        ctx: &mut Context,
        program: &str,
        args: &[String],
        cwd: &Path,
    ) -> Result<RunningService<RoleClient, ClientConfig>, BoxError> {
        let mut command = tokio::process::Command::new(program);
        command.args(args).current_dir(cwd);
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
        let tools = self.list_tools(&peer, id)?;
        match args.value("TOOL") {
            None if tools.is_empty() => ctx.print("The server has no tools."),
            None => {
                let rows = (tools.iter()).map(|tool| [label(&*tool.name), label(summary(tool))]);
                let table = table_horizontal(rows.collect::<Vec<_>>());
                ctx.print_island(IslandBuilder::new(table).freeze()?);
            }
            Some(name) => {
                ctx.print(describe_tool(find_tool(&tools, name)?));
            }
        }
        Ok(())
    }

    fn call(&self, ctx: &mut Context, args: &Matches) -> CommandResult {
        let name = args.value("TOOL").ok_or("no tool")?;
        let json = args.values("JSON");
        let (peer, id) = self.peer()?;
        let arguments = if json.is_empty() {
            let tools = self.list_tools(&peer, id)?;
            match call::ask(ctx, find_tool(&tools, name)?)? {
                Some(arguments) => arguments,
                None => {
                    ctx.print("Not called.");
                    return Ok(());
                }
            }
        } else {
            // Not checked against the tool's schema: that's the server's job.
            call::parse_object(&json.join(" "))?
        };
        let params = CallToolRequestParams::new(name.to_owned()).with_arguments(arguments);
        let result = match self.request(id, peer.call_tool_once(params))? {
            CallToolResponse::Complete(result) => result,
            CallToolResponse::InputRequired(_) => {
                return Err("the server asked for more input, which isn't supported yet".into());
            }
            CallToolResponse::Task(_) => {
                return Err("the server made the call a task, which isn't supported yet".into());
            }
            _ => return Err("the server's response is of an unknown kind".into()),
        };
        call::show_result(ctx, name, &result)
    }

    fn resources(&self, ctx: &mut Context, _: &Matches) -> CommandResult {
        let (peer, id) = self.peer()?;
        let resources = self.list_resources(&peer, id)?;
        let templates = match self.request(id, peer.list_all_resource_templates()) {
            Ok(templates) => templates,
            Err(e) => {
                ctx.warn(format!("can't list the resource templates: {e}"));
                Vec::new()
            }
        };
        if resources.is_empty() && templates.is_empty() {
            ctx.print("The server has no resources.");
            return Ok(());
        }
        let rows = (resources.iter())
            .map(|r| [label(&*r.uri), label(r.title.as_deref().unwrap_or(&r.name))]);
        let resources = table_horizontal(rows.collect::<Vec<_>>());
        let island = if templates.is_empty() {
            resources
        } else {
            let rows = (templates.iter()).map(|t| {
                [
                    label(&*t.uri_template),
                    label(t.title.as_deref().unwrap_or(&t.name)),
                ]
            });
            column([
                label("Resources:"),
                resources,
                label("Templates:"),
                table_horizontal(rows.collect::<Vec<_>>()),
            ])
        };
        ctx.print_island(IslandBuilder::new(island).freeze()?);
        Ok(())
    }

    fn read(&self, ctx: &mut Context, args: &Matches) -> CommandResult {
        let uri = args.value("URI").ok_or("no URI")?;
        let (peer, id) = self.peer()?;
        require(&peer, "resources", |c| c.resources.is_some())?;
        let params = ReadResourceRequestParams::new(uri);
        let result = match self.request(id, peer.read_resource_once(params))? {
            ReadResourceResponse::Complete(result) => result,
            ReadResourceResponse::InputRequired(_) => {
                return Err("the server asked for more input, which isn't supported yet".into());
            }
            _ => return Err("the server's response is of an unknown kind".into()),
        };
        if result.contents.is_empty() {
            ctx.print("The resource is empty.");
        }
        // Several contents (e.g. a directory's files) are told apart by URI.
        let several = result.contents.len() > 1;
        for contents in &result.contents {
            let heading = several.then(|| format!("{}:", content::uri(contents)));
            content::show_resource(ctx, heading.as_deref(), contents);
        }
        Ok(())
    }

    fn prompts(&self, ctx: &mut Context, args: &Matches) -> CommandResult {
        let (peer, id) = self.peer()?;
        let prompts = self.list_prompts(&peer, id)?;
        match args.value("PROMPT") {
            None if prompts.is_empty() => ctx.print("The server has no prompts."),
            None => {
                let rows = (prompts.iter()).map(|p| [label(&*p.name), label(prompt_summary(p))]);
                let table = table_horizontal(rows.collect::<Vec<_>>());
                ctx.print_island(IslandBuilder::new(table).freeze()?);
            }
            Some(name) => ctx.print(prompt::describe(find_prompt(&prompts, name)?)),
        }
        Ok(())
    }

    fn prompt(&self, ctx: &mut Context, args: &Matches) -> CommandResult {
        let name = args.value("PROMPT").ok_or("no prompt")?;
        let given = args.values("ARG");
        let (peer, id) = self.peer()?;
        let arguments = if given.is_empty() {
            let prompts = self.list_prompts(&peer, id)?;
            match prompt::ask(ctx, find_prompt(&prompts, name)?)? {
                Some(arguments) => arguments,
                None => {
                    ctx.print("Cancelled.");
                    return Ok(());
                }
            }
        } else {
            prompt::parse_assignments(given)?
        };
        let params = GetPromptRequestParams::new(name).with_arguments(arguments);
        let result = match self.request(id, peer.get_prompt_once(params))? {
            GetPromptResponse::Complete(result) => result,
            GetPromptResponse::InputRequired(_) => {
                return Err("the server asked for more input, which isn't supported yet".into());
            }
            _ => return Err("the server's response is of an unknown kind".into()),
        };
        prompt::show(ctx, name, &result);
        Ok(())
    }

    /// Lists the server's tools (and remembers them for completion).
    fn list_tools(&self, peer: &Peer<RoleClient>, id: u64) -> Result<Vec<Tool>, BoxError> {
        require(peer, "tools", |c| c.tools.is_some())?;
        let tools = self.request(id, peer.list_all_tools())?;
        lock(&self.names).tools = tools.iter().map(|t| t.name.to_string()).collect();
        Ok(tools)
    }

    /// Lists the server's resources (and remembers them for completion).
    fn list_resources(&self, peer: &Peer<RoleClient>, id: u64) -> Result<Vec<Resource>, BoxError> {
        require(peer, "resources", |c| c.resources.is_some())?;
        let resources = self.request(id, peer.list_all_resources())?;
        lock(&self.names).resources = resources.iter().map(|r| r.uri.clone()).collect();
        Ok(resources)
    }

    /// Lists the server's prompts (and remembers them for completion).
    fn list_prompts(&self, peer: &Peer<RoleClient>, id: u64) -> Result<Vec<Prompt>, BoxError> {
        require(peer, "prompts", |c| c.prompts.is_some())?;
        let prompts = self.request(id, peer.list_all_prompts())?;
        lock(&self.names).prompts = prompts.iter().map(|p| p.name.clone()).collect();
        Ok(prompts)
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
                    *lock(&self.names) = Names::default();
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
    let c = client.clone();
    interpreter.register_spec(
        "reconnect",
        "Close the connection, if any, and connect to the last server again",
        Spec::new(),
        move |ctx, args| c.reconnect(ctx, args),
    );
    let c = client.clone();
    interpreter.register_spec(
        "tools",
        "List the server's tools, or describe one",
        Spec::new()
            .arg("TOOL", names_kind(client, |n| &n.tools))
            .optional(),
        move |ctx, args| c.tools(ctx, args),
    );
    let spec = Spec::new()
        .arg("TOOL", names_kind(client, |n| &n.tools))
        .arg("JSON", Kind::Text)
        .optional()
        .repeated();
    let c = client.clone();
    interpreter.register_spec(
        "call",
        "Call a tool: fill in its arguments, or give them as a JSON object",
        spec,
        move |ctx, args| c.call(ctx, args),
    );
    let c = client.clone();
    interpreter.register_spec(
        "resources",
        "List the server's resources and resource templates",
        Spec::new(),
        move |ctx, args| c.resources(ctx, args),
    );
    let c = client.clone();
    interpreter.register_spec(
        "read",
        "Read a resource",
        Spec::new().arg("URI", names_kind(client, |n| &n.resources)),
        move |ctx, args| c.read(ctx, args),
    );
    let c = client.clone();
    interpreter.register_spec(
        "prompts",
        "List the server's prompts, or describe one",
        Spec::new()
            .arg("PROMPT", names_kind(client, |n| &n.prompts))
            .optional(),
        move |ctx, args| c.prompts(ctx, args),
    );
    let spec = Spec::new()
        .arg("PROMPT", names_kind(client, |n| &n.prompts))
        .arg("ARG", Kind::Text)
        .optional()
        .repeated();
    let c = client.clone();
    interpreter.register_spec(
        "prompt",
        "Get a prompt: fill in its arguments, or give them as NAME=VALUE",
        spec,
        move |ctx, args| c.prompt(ctx, args),
    );
}

/// Names completed from those last listed (`which` of them).
fn names_kind(client: &Client, which: fn(&Names) -> &Vec<String>) -> Kind {
    let names = client.names.clone();
    Kind::custom(move |_| which(&lock(&names)).clone())
}

/// Fails unless the server offers `what` (by `offered`, from its
/// capabilities).
fn require(
    peer: &Peer<RoleClient>,
    what: &str,
    offered: fn(&ServerCapabilities) -> bool,
) -> Result<(), BoxError> {
    match peer
        .peer_info()
        .is_some_and(|info| offered(&info.capabilities))
    {
        true => Ok(()),
        false => Err(format!("the server doesn't offer {what}").into()),
    }
}

fn find_prompt<'a>(prompts: &'a [Prompt], name: &str) -> Result<&'a Prompt, BoxError> {
    let prompt = prompts.iter().find(|prompt| prompt.name == name);
    prompt.ok_or_else(|| format!("no prompt {name:?}").into())
}

/// A prompt in one line: its title, or the first line of its description.
fn prompt_summary(prompt: &Prompt) -> String {
    let description = prompt.description.as_deref().and_then(|d| d.lines().next());
    (prompt.title.as_deref().or(description))
        .unwrap_or_default()
        .to_owned()
}

fn find_tool<'a>(tools: &'a [Tool], name: &str) -> Result<&'a Tool, BoxError> {
    let tool = tools.iter().find(|tool| tool.name == name);
    tool.ok_or_else(|| format!("no tool {name:?}").into())
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
