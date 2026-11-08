//! `hob-mcp`: an MCP server over stdio that lets an agent use hob.
//!
//! It starts `hob --headless` from the same directory, or the command given
//! as arguments. With `--allow-restart`, the agent can close hob and start
//! it again; otherwise, once hob has exited, so does hob-mcp. With
//! `--agent-spawns` (which implies `--allow-restart`), it starts nothing:
//! the agent starts hob with a command it gives.

use std::ffi::OsString;
use std::process::ExitCode;

use hob_mcp::{Bridge, Launch, Limits};

/// The program name that starts lines logged to stderr.
const LOG_PREFIX: &str = "hob-mcp";

const USAGE: &str = "usage: hob-mcp [--allow-restart] [--agent-spawns | COMMAND [ARG]...]";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1).peekable();
    let (mut restart, mut by_agent) = (false, false);
    while let Some(flag) = args.next_if(|a| a.to_string_lossy().starts_with("--")) {
        match flag.to_str() {
            Some("--allow-restart") => restart = true,
            Some("--agent-spawns") => by_agent = true,
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let command: Vec<OsString> = args.collect();
    let launch = if by_agent {
        if !command.is_empty() {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
        Launch::ByAgent
    } else if !command.is_empty() {
        Launch::Command { command, restart }
    } else {
        match std::env::current_exe() {
            Ok(exe) => {
                let command = vec![exe.with_file_name("hob").into(), "--headless".into()];
                Launch::Command { command, restart }
            }
            Err(e) => {
                eprintln!("{LOG_PREFIX}: can't find hob: {e}");
                return ExitCode::FAILURE;
            }
        }
    };
    let mut bridge = Bridge::new(launch.clone(), Limits::default());
    if let Launch::Command { .. } = launch
        && let Err(report) = bridge.start(None)
    {
        eprint!("{LOG_PREFIX}: {report}");
        return ExitCode::FAILURE;
    }
    match hob_mcp::mcp::serve(&mut bridge, std::io::stdin(), std::io::stdout()) {
        Ok(()) if bridge.is_over() => {
            eprintln!("{LOG_PREFIX}: hob has exited");
            ExitCode::FAILURE
        }
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{LOG_PREFIX}: {e}");
            ExitCode::FAILURE
        }
    }
}
