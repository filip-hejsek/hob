//! `hob-mcp`: an MCP server over stdio that lets an agent use hob.
//!
//! It starts `hob --headless` from the same directory, or the command given
//! as arguments. With `--agent-spawns`, it starts nothing: the agent starts
//! hob with a command it gives.

use std::ffi::OsString;
use std::process::ExitCode;

use hob_mcp::{Bridge, Launch, Limits};

const USAGE: &str = "usage: hob-mcp [--agent-spawns | COMMAND [ARG]...]";

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let launch = match args.first() {
        Some(flag) if flag == "--agent-spawns" && args.len() == 1 => Launch::ByAgent,
        Some(flag) if flag.to_string_lossy().starts_with("--") => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
        Some(_) => Launch::Command(args),
        None => match std::env::current_exe() {
            Ok(exe) => Launch::Command(vec![exe.with_file_name("hob").into(), "--headless".into()]),
            Err(e) => {
                eprintln!("hob-mcp: can't find hob: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    let mut bridge = Bridge::new(launch.clone(), Limits::default());
    if let Launch::Command(_) = launch
        && let Err(report) = bridge.start(None)
    {
        eprint!("hob-mcp: {report}");
        return ExitCode::FAILURE;
    }
    match hob_mcp::mcp::serve(&mut bridge, std::io::stdin(), std::io::stdout()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hob-mcp: {e}");
            ExitCode::FAILURE
        }
    }
}
