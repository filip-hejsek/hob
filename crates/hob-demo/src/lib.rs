//! Demo commands for hob. Between them, they use everything a command can do
//! through `hob-core`.

use std::thread;
use std::time::Duration;

use hob_core::args::{Kind, Matches, Spec};
use hob_core::{CommandResult, Context, Interpreter};
use kettle_ui::widget::*;
use kettle_ui::{Image, IslandBuilder, Outcome, Value};
use kettle_ui_demo::Demo;

/// Registers all demo commands.
pub fn register(interpreter: &mut Interpreter) {
    register_islands(interpreter);
    interpreter.register("echo", "Print the arguments", echo);
    interpreter.register_spec(
        "count",
        "Count to N (5 by default), slowly",
        Spec::new().arg("N", Kind::Text).optional(),
        count,
    );
    interpreter.register("fail", "Fail with the arguments as the error", fail);
    interpreter.register_spec("panic", "Panic", Spec::new(), |_, _| {
        panic!("the demo panicked")
    });
    interpreter.register_spec(
        "invalid",
        "Show an island that refers to missing state",
        Spec::new(),
        invalid,
    );
    interpreter.register(
        "pizza",
        "Order a pizza; the arguments fill in the name",
        pizza,
    );
    interpreter.register_spec("levels", "Log a message at each level", Spec::new(), levels);
    interpreter.register_spec(
        "view",
        "Show image files (-e: not saved in the session; -l: only their paths saved)",
        Spec::new()
            .flag("ephemeral", "Don't save the images in the session")
            .short('e')
            .flag(
                "link",
                "Save only the files' paths in the session, and show them if unchanged",
            )
            .short('l')
            .arg("FILE", Kind::Path)
            .repeated(),
        view,
    );
    interpreter.register_spec(
        "swatch",
        "Show a field for a color, like #ff8800, and a swatch of it",
        Spec::new(),
        swatch,
    );
    interpreter.register_spec(
        "table",
        "Show a table, with all borders and with horizontal ones only",
        Spec::new(),
        table_demo,
    );
    interpreter.register(
        "confirm",
        "Ask a yes/no question (the arguments), answered with y or n",
        confirm,
    );
}

/// Registers a command for each demo island. It shows the island and prints
/// what the user submitted.
fn register_islands(interpreter: &mut Interpreter) {
    for demo in Demo::ALL {
        let name = match demo {
            Demo::Contact => "contact",
            Demo::FruitSearch => "fruit-search",
        };
        let description = format!("Show the {} demo island", demo.title().to_lowercase());
        interpreter.register_spec(name, description, Spec::new(), move |ctx, _| {
            let title = demo.title();
            let message = match ctx.show(demo.builder())? {
                Outcome::Submitted(fields) => format!("{title}: {}", demo.summary(&fields)),
                Outcome::Cancelled => format!("{title}: cancelled"),
                Outcome::Pressed(id, _) => format!("{title}: pressed {id}"),
            };
            ctx.print(message);
            Ok(())
        });
    }
}

/// Prints its arguments.
fn echo(ctx: &mut Context, args: &[String]) -> CommandResult {
    ctx.print(args.join(" "));
    Ok(())
}

/// Counts to N (5 by default), slowly. Shows output arriving while a command
/// runs, and several commands running at once.
fn count(ctx: &mut Context, args: &Matches) -> CommandResult {
    let n: u32 = args.parse("N")?.unwrap_or(5);
    for i in 1..=n {
        if i > 1 {
            thread::sleep(Duration::from_millis(500));
        }
        ctx.print(i.to_string());
    }
    Ok(())
}

/// Fails with its arguments as the error message.
fn fail(ctx: &mut Context, args: &[String]) -> CommandResult {
    ctx.print("About to fail…");
    match args {
        [] => Err("something went wrong".into()),
        _ => Err(args.join(" ").into()),
    }
}

/// Logs a message at each level.
fn levels(ctx: &mut Context, _: &Matches) -> CommandResult {
    ctx.error("an error");
    ctx.warn("a warning");
    ctx.info("some information");
    ctx.debug("a debug message (hidden unless HOB_LOG=debug)");
    Ok(())
}

/// Asks for confirmation. "Yes" isn't a submit button, so Enter can't
/// confirm by accident.
fn confirm(ctx: &mut Context, args: &[String]) -> CommandResult {
    let question = match args {
        [] => "Are you sure?".to_owned(),
        _ => args.join(" "),
    };
    let island = IslandBuilder::new(column([
        label(question),
        row([button_danger("yes", "Yes [y]"), cancel_button("No [n]")]),
    ]));
    let answer = match ctx.show(island)? {
        Outcome::Pressed(..) => "yes",
        Outcome::Submitted(_) | Outcome::Cancelled => "no",
    };
    ctx.print(format!("Answer: {answer}"));
    Ok(())
}

fn invalid(ctx: &mut Context, _: &Matches) -> CommandResult {
    ctx.show(IslandBuilder::new(label(state("nonexistent"))))?;
    Ok(())
}

/// The toppings: field id, label, price.
const TOPPINGS: [(&str, &str, u32); 4] = [
    ("cheese", "Extra cheese", 100),
    ("mushrooms", "Mushrooms", 150),
    ("olives", "Olives", 100),
    ("ham", "Ham", 200),
];

/// The price of a pizza without toppings, in cents.
const BASE_PRICE: u32 = 800;

/// Orders a pizza in several steps, then prints a receipt. The arguments are
/// the name to prefill.
fn pizza(ctx: &mut Context, args: &[String]) -> CommandResult {
    let Outcome::Submitted(fields) = ctx.show(toppings_island())? else {
        ctx.print("Order cancelled.");
        return Ok(());
    };
    let toppings: Vec<_> = TOPPINGS
        .iter()
        .filter(|(id, ..)| fields.checked(id))
        .collect();

    let Outcome::Submitted(fields) = ctx.show(delivery_island(&args.join(" ")))? else {
        ctx.print("Order cancelled.");
        return Ok(());
    };
    let name = fields.text("name").trim();
    let address = fields.text("address").trim();

    let mut items = vec![format!("Pizza  {}", price(BASE_PRICE))];
    items.extend(
        toppings
            .iter()
            .map(|(_, label, cents)| format!("{label}  {}", price(*cents))),
    );
    let total = BASE_PRICE + toppings.iter().map(|(.., cents)| cents).sum::<u32>();
    let mut receipt = IslandBuilder::new(column([
        label(format!("Receipt for {name}")),
        list("items"),
        label(format!("Total: {}", price(total))),
        label(format!("Delivering to {address}")),
    ]));
    receipt.set_state("items", items);
    ctx.print_island(receipt.freeze()?);
    Ok(())
}

fn toppings_island() -> IslandBuilder {
    let mut children = vec![label("Step 1 of 2: choose toppings")];
    children.extend(TOPPINGS.iter().map(|(id, label, _)| checkbox(id, label)));
    children.push(label(state("price")));
    children.push(row([submit_button("Next"), cancel_button("Cancel")]));
    let mut builder = IslandBuilder::new(column(children));
    builder.derive("price", |f| {
        let toppings: u32 = TOPPINGS
            .iter()
            .filter(|(id, ..)| f.checked(id))
            .map(|(.., cents)| cents)
            .sum();
        format!("Price: {}", price(BASE_PRICE + toppings)).into()
    });
    builder
}

/// Asks for the name (prefilled with `name`) and address; both are required.
fn delivery_island(name: &str) -> IslandBuilder {
    let mut builder = IslandBuilder::new(column([
        label("Step 2 of 2: delivery"),
        text_field("name", "Name"),
        label(state("name_error")),
        text_field("address", "Address"),
        label(state("address_error")),
        row([submit_button("Order"), cancel_button("Cancel")]),
    ]));
    builder.set_text("name", name);
    builder.validate("name_error", |f| {
        f.text("name")
            .trim()
            .is_empty()
            .then(|| "Enter your name.".into())
    });
    builder.validate("address_error", |f| {
        (f.text("address").trim().is_empty()).then(|| "Enter the delivery address.".into())
    });
    builder
}

fn price(cents: u32) -> String {
    format!("€{}.{:02}", cents / 100, cents % 100)
}

/// Prints the image files given (relative to the working directory), each
/// with its name. With `-e`, the images are ephemeral: not saved in the
/// session. With `-l`, they're linked: only their paths are saved.
fn view(ctx: &mut Context, args: &Matches) -> CommandResult {
    let (ephemeral, link) = (args.flag("ephemeral"), args.flag("link"));
    if ephemeral && link {
        return Err("-e and -l can't be used together".into());
    }
    for file in args.values("FILE") {
        let picture = match Image::from_file(ctx.resolve(file), file.as_str()) {
            Ok(picture) if ephemeral => picture.ephemeral(),
            Ok(picture) if link => picture.linked(),
            Ok(picture) => picture,
            Err(e) => {
                ctx.error(format!("{file}: {e}"));
                continue;
            }
        };
        let mut b = IslandBuilder::new(column([label(file.as_str()), image("image")]));
        b.set_state("image", picture);
        ctx.print_island(b.freeze()?);
    }
    Ok(())
}

/// Shows a swatch of the color typed, made as the user types: an image in
/// derived state.
fn swatch(ctx: &mut Context, _: &Matches) -> CommandResult {
    let mut b = IslandBuilder::new(column([
        text_field("color", "A color, like #ff8800"),
        image("swatch"),
        submit_button("OK"),
    ]));
    b.derive("swatch", |f| {
        let color = f.text("color");
        let Some([r, g, b]) = parse_color(color) else {
            return Value::Text(String::new());
        };
        let pixels = [r, g, b, 255].repeat(120 * 40);
        match Image::from_rgba(120, 40, &pixels, format!("The color {}", color.trim())) {
            Ok(image) => image.into(),
            Err(_) => Value::Text(String::new()),
        }
    });
    if let Outcome::Submitted(fields) = ctx.show(b)? {
        ctx.print(format!("Color: {}", fields.text("color")));
    }
    Ok(())
}

/// Prints the same table twice: with all borders, and with horizontal ones
/// only.
fn table_demo(ctx: &mut Context, _: &Matches) -> CommandResult {
    let planets = [
        ["Planet", "Moons", "Distance (AU)"],
        ["Mercury", "0", "0.39"],
        ["Venus", "0", "0.72"],
        ["Earth", "1", "1.00"],
        ["Mars", "2", "1.52"],
    ];
    let rows = || planets.map(|cells| cells.map(label));
    let b = IslandBuilder::new(column([
        label("All borders"),
        table(rows()),
        label("Horizontal borders only"),
        table_horizontal(rows()),
    ]));
    ctx.print_island(b.freeze()?);
    Ok(())
}

/// Parses `#rrggbb`.
fn parse_color(s: &str) -> Option<[u8; 3]> {
    let hex = s.trim().strip_prefix('#')?;
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}
