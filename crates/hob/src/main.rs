//! The hob desktop application.

use std::path::PathBuf;

use hob_core::Interpreter;
use kettle_ui::Outcome;
use kettle_ui_demo::Demo;

fn main() -> iced::Result {
    let mut interpreter = Interpreter::new();
    register_demos(&mut interpreter);
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

/// Registers a command for each demo island. It shows the island and prints
/// what the user submitted.
fn register_demos(interpreter: &mut Interpreter) {
    for demo in Demo::ALL {
        let name = match demo {
            Demo::Contact => "contact",
            Demo::FruitSearch => "fruit-search",
        };
        interpreter.register(name, move |ctx, args| {
            if !args.is_empty() {
                return Err("takes no arguments".into());
            }
            let title = demo.title();
            let message = match ctx.show(demo.builder())? {
                Outcome::Submitted(fields) => format!("{title}: {}", demo.summary(&fields)),
                Outcome::Cancelled => format!("{title}: cancelled"),
            };
            ctx.print(message);
            Ok(())
        });
    }
}
