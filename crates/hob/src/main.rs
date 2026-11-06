//! The hob desktop application. With `--headless`, it runs without a window
//! instead, for a client program that talks JSON-RPC over stdin and stdout
//! (see `hob-headless`).

use std::path::PathBuf;
use std::process::ExitCode;

use hob_core::Interpreter;

const USAGE: &str = "usage: hob [--headless]";

fn main() -> ExitCode {
    let mut interpreter = Interpreter::new();
    hob_core::builtins::register(&mut interpreter);
    hob_demo::register(&mut interpreter);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [] => {
            let placeholder = "Type a command, or help";
            hob_gui::run(interpreter, session_path(), placeholder).map_err(|e| e.to_string())
        }
        [flag] if flag == "--headless" => {
            hob_headless::serve(interpreter, std::io::stdin(), std::io::stdout())
                .map_err(|e| e.to_string())
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hob: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Where the session is saved: `HOB_SESSION` if set, otherwise in the
/// platform's state (or local data) directory.
fn session_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("HOB_SESSION") {
        return Some(path.into());
    }
    let dir = dirs::state_dir().or_else(dirs::data_local_dir)?;
    Some(dir.join("hob").join("session.json"))
}
