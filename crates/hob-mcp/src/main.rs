//! `hob-mcp`: an MCP server over stdio that lets an agent use hob.
//!
//! It starts `hob --headless` from the same directory, or the command given
//! as arguments.

use std::process::ExitCode;
use std::sync::mpsc;

use hob_headless::client::Client;
use hob_mcp::Bridge;

fn main() -> ExitCode {
    let started = Client::command(std::env::args_os().skip(1)).and_then(|command| {
        let (sender, notifications) = mpsc::channel();
        let notify = move |n| {
            let _ = sender.send(n);
        };
        Ok((Client::spawn(command, notify)?, notifications))
    });
    let (client, notifications) = match started {
        Ok(started) => started,
        Err(e) => {
            eprintln!("hob-mcp: couldn't start hob: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut bridge = Bridge::new(client, notifications);
    match hob_mcp::mcp::serve(&mut bridge, std::io::stdin(), std::io::stdout()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hob-mcp: {e}");
            ExitCode::FAILURE
        }
    }
}
