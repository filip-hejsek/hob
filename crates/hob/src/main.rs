//! The hob desktop application. With `--headless`, it runs without a window
//! instead, for a client program that talks JSON-RPC over stdin and stdout
//! (see `hob-headless`). With `--preview`, it shows a saved session
//! read-only, without running commands.

use std::path::PathBuf;
use std::process::ExitCode;

use hob_core::Interpreter;

const USAGE: &str = "usage: hob [--headless | --preview [SESSION]]";

fn main() -> ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let result = match args.as_slice() {
        [] => {
            let placeholder = "Type a command, or help";
            hob_gui::run(interpreter(), session_path(), placeholder).map_err(|e| e.to_string())
        }
        // No interpreter: a preview runs nothing.
        [flag, path @ ..] if flag == "--preview" && path.len() <= 1 => {
            let path = match path {
                [path] => Some(PathBuf::from(path)),
                _ => session_path(),
            };
            match path {
                Some(path) => hob_gui::preview(path).map_err(|e| e.to_string()),
                None => Err("no session to preview: no state directory".to_owned()),
            }
        }
        [flag] if flag == "--headless" => {
            let interpreter = interpreter();
            let mut options = hob_headless::Options::default();
            options.image_paths = std::env::var_os("HOB_IMAGE_PATHS").is_some_and(|v| v == "1");
            hob_headless::serve_with(interpreter, std::io::stdin(), std::io::stdout(), options)
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

/// An interpreter with all the commands.
fn interpreter() -> Interpreter {
    let mut interpreter = Interpreter::new();
    hob_core::builtins::register(&mut interpreter);
    hob_demo::register(&mut interpreter);
    hob_fs::register(&mut interpreter);
    interpreter
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
