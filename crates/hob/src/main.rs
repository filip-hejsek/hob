//! The hob desktop application.

use std::path::PathBuf;

use hob_core::Interpreter;

fn main() -> iced::Result {
    let mut interpreter = Interpreter::new();
    hob_demo::register(&mut interpreter);
    hob_gui::run(interpreter, session_path())
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
