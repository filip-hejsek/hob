//! Demo commands for hob.

use hob_core::Interpreter;
use kettle_ui::Outcome;
use kettle_ui_demo::Demo;

/// Registers all demo commands.
pub fn register(interpreter: &mut Interpreter) {
    register_islands(interpreter);
}

/// Registers a command for each demo island. It shows the island and prints
/// what the user submitted.
fn register_islands(interpreter: &mut Interpreter) {
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
