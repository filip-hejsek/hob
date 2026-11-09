//! An MCP client for people: lets a user try an MCP server the way an agent
//! would, with hob as the GUI. It has hob's generic builtins
//! (`hob_core::builtins`, without the shell command) and the client's
//! commands (see the library), but none of hob's own commands.

use std::path::PathBuf;
use std::process::ExitCode;

use hob_core::Interpreter;
use hob_mcp_test_client::Client;

/// The program name that starts lines logged to stderr.
const LOG_PREFIX: &str = "hob-mcp-test-client";

fn main() -> ExitCode {
    if std::env::args_os().len() > 1 {
        eprintln!("usage: hob-mcp-test-client");
        return ExitCode::from(2);
    }
    // Commands block on their own threads; they reach rmcp's async API
    // through this runtime.
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("{LOG_PREFIX}: can't start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let client = Client::new(runtime.handle().clone());
    let mut interpreter = Interpreter::named("hob-mcp-test-client");
    hob_core::builtins::register(&mut interpreter);
    hob_mcp_test_client::register(&mut interpreter, &client);
    let result = hob_gui::run(interpreter, session_path(), gui_config());
    // Ends a server process, which dropping the runtime might not.
    client.close();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{LOG_PREFIX}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// What the shell calls the client: its own name and application ID, so
/// desktops don't group it with hob.
fn gui_config() -> hob_gui::GuiConfig {
    hob_gui::GuiConfig {
        title: "hob-mcp-test-client".to_owned(),
        app_id: "io.github.filip_hejsek.hob_mcp_test_client".to_owned(),
        command_placeholder: "Type a command, or help".to_owned(),
        log_prefix: LOG_PREFIX.to_owned(),
    }
}

/// Where the session is saved: `HOB_MCP_TEST_CLIENT_SESSION` if set,
/// otherwise in the platform's state (or local data) directory. Kept apart
/// from hob's own session.
fn session_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("HOB_MCP_TEST_CLIENT_SESSION") {
        return Some(path.into());
    }
    let dir = dirs::state_dir().or_else(dirs::data_local_dir)?;
    Some(dir.join("hob-mcp-test-client").join("session.json"))
}
