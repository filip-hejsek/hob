//! An MCP client for people: lets a user try an MCP server the way an agent
//! would, with hob as the GUI. It has hob's generic builtins
//! (`hob_core::builtins`), but none of hob's own commands.

use std::path::PathBuf;
use std::process::ExitCode;

use hob_core::Interpreter;

fn main() -> ExitCode {
    if std::env::args_os().len() > 1 {
        eprintln!("usage: hob-mcp-test-client");
        return ExitCode::from(2);
    }
    // Commands block on their own threads; they'll reach rmcp's async API
    // through this runtime.
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("hob-mcp-test-client: can't start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let interpreter = interpreter(runtime.handle().clone());
    let placeholder = "Type a command, or help";
    match hob_gui::run(interpreter, session_path(), placeholder) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hob-mcp-test-client: {e}");
            ExitCode::FAILURE
        }
    }
}

/// An interpreter with the client's commands.
fn interpreter(_runtime: tokio::runtime::Handle) -> Interpreter {
    let mut interpreter = Interpreter::new();
    hob_core::builtins::register(&mut interpreter);
    interpreter
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
