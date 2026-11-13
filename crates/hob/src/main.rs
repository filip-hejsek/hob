//! The hob desktop application. With `--headless`, it runs without a window
//! instead, for a client program that talks JSON-RPC over stdin and stdout
//! (see `hob-headless`). With `--preview`, it shows a saved session
//! read-only, without running commands. Built without the `gui` feature, it
//! runs only with `--headless`. The `demo`, `fs`, `git` and `notes` features
//! add the commands of the crates of those names.

#[cfg(any(feature = "gui", feature = "notes"))]
use std::path::PathBuf;
use std::process::ExitCode;

use hob_core::Interpreter;

/// The program's name, as shown to the user.
const NAME: &str = "hob";

/// The program name that starts lines logged to stderr.
const LOG_PREFIX: &str = NAME;

#[cfg(feature = "gui")]
const USAGE: &str = "usage: hob [--headless | --preview [SESSION] | --version]";
#[cfg(not(feature = "gui"))]
const USAGE: &str = "usage: hob --headless | --version (built without the gui feature)";

/// What `--version` prints: the version and the Git commit built from.
const VERSION: &str = concat!(
    env!("CARGO_BIN_NAME"),
    " ",
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("HOB_COMMIT"),
    ")"
);

fn main() -> ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let result = match args.as_slice() {
        #[cfg(feature = "gui")]
        [] => hob_gui::run(interpreter(), session_path(), gui_config()).map_err(|e| e.to_string()),
        // No interpreter: a preview runs nothing.
        #[cfg(feature = "gui")]
        [flag, path @ ..] if flag == "--preview" && path.len() <= 1 => {
            let path = match path {
                [path] => Some(PathBuf::from(path)),
                _ => session_path(),
            };
            match path {
                Some(path) => hob_gui::preview(path, gui_config()).map_err(|e| e.to_string()),
                None => Err("no session to preview: no state directory".to_owned()),
            }
        }
        [flag] if flag == "--version" => {
            println!("{VERSION}");
            Ok(())
        }
        [flag] if flag == "--headless" => {
            let interpreter = interpreter();
            let mut options = hob_headless::Options::default();
            options.image_paths = std::env::var_os("HOB_IMAGE_PATHS").is_some_and(|v| v == "1");
            options.log_prefix = LOG_PREFIX.to_owned();
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
            eprintln!("{LOG_PREFIX}: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(feature = "gui")]
/// The application ID: the window's Wayland app ID and X11 class, which
/// must match the name of the desktop file (`data/<APP_ID>.desktop`).
const APP_ID: &str = "io.github.filip_hejsek.hob";

#[cfg(feature = "gui")]
/// What the shell calls hob.
fn gui_config() -> hob_gui::GuiConfig {
    hob_gui::GuiConfig {
        title: NAME.to_owned(),
        app_id: APP_ID.to_owned(),
        command_placeholder: "Type a command, or help".into(),
        log_prefix: LOG_PREFIX.to_owned(),
        startup: Vec::new(),
    }
}

/// An interpreter with the built-in commands and those of the enabled
/// features.
fn interpreter() -> Interpreter {
    let mut interpreter = Interpreter::named(NAME);
    hob_core::builtins::register(&mut interpreter);
    hob_core::builtins::register_shell(&mut interpreter);
    #[cfg(feature = "demo")]
    hob_demo::register(&mut interpreter);
    #[cfg(feature = "fs")]
    hob_fs::register(&mut interpreter);
    #[cfg(feature = "git")]
    hob_git::register(&mut interpreter);
    #[cfg(feature = "notes")]
    hob_notes::register(&mut interpreter, notes_path());
    interpreter
}

#[cfg(feature = "notes")]
/// Where the notes are kept: `HOB_NOTES` if set, otherwise in the
/// platform's data directory.
fn notes_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("HOB_NOTES") {
        return Some(path.into());
    }
    Some(dirs::data_dir()?.join("hob").join("notes.sqlite"))
}

#[cfg(feature = "gui")]
/// Where the session is saved: `HOB_SESSION` if set, otherwise in the
/// platform's state (or local data) directory.
fn session_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("HOB_SESSION") {
        return Some(path.into());
    }
    let dir = dirs::state_dir().or_else(dirs::data_local_dir)?;
    Some(dir.join("hob").join("session.json"))
}
