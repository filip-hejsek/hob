//! Demo commands for hob. Between them, they use everything a command can do
//! through `hob-core`.

use std::thread;
use std::time::Duration;

use hob_core::{CommandResult, Context, Interpreter};
use kettle_ui::widget::*;
use kettle_ui::{IslandBuilder, Outcome};
use kettle_ui_demo::Demo;

/// Registers all demo commands.
pub fn register(interpreter: &mut Interpreter) {
    register_islands(interpreter);
    interpreter.register("echo", "Print the arguments", echo);
    interpreter.register("count", "Count to N (5 by default), slowly", count);
    interpreter.register("fail", "Fail with the arguments as the error", fail);
    interpreter.register("panic", "Panic", |_, _| panic!("the demo panicked"));
    interpreter.register(
        "invalid",
        "Show an island that refers to missing state",
        invalid,
    );
    interpreter.register(
        "pizza",
        "Order a pizza; the arguments fill in the name",
        pizza,
    );
    interpreter.register("levels", "Log a message at each level", levels);
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
        interpreter.register(name, description, move |ctx, args| {
            no_args(args)?;
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

fn no_args(args: &[String]) -> CommandResult {
    match args {
        [] => Ok(()),
        _ => Err("takes no arguments".into()),
    }
}

/// Prints its arguments.
fn echo(ctx: &mut Context, args: &[String]) -> CommandResult {
    ctx.print(args.join(" "));
    Ok(())
}

/// Counts to N (5 by default), slowly. Shows output arriving while a command
/// runs, and several commands running at once.
fn count(ctx: &mut Context, args: &[String]) -> CommandResult {
    let n: u32 = match args {
        [] => 5,
        [n] => n.parse().map_err(|_| format!("not a number: {n:?}"))?,
        _ => return Err("usage: count [N]".into()),
    };
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
fn levels(ctx: &mut Context, args: &[String]) -> CommandResult {
    no_args(args)?;
    ctx.error("an error");
    ctx.warn("a warning");
    ctx.info("some information");
    ctx.debug("a debug message (hidden unless HOB_LOG=debug)");
    Ok(())
}

/// Shows an island that refers to state that doesn't exist.
fn confirm(ctx: &mut Context, args: &[String]) -> CommandResult {
    let question = match args {
        [] => "Are you sure?".to_owned(),
        _ => args.join(" "),
    };
    let island = IslandBuilder::new(column([
        label(question),
        row([submit_button("Yes [y]"), cancel_button("No [n]")]),
    ]));
    let answer = match ctx.show(island)? {
        Outcome::Submitted(_) => "yes",
        Outcome::Cancelled => "no",
    };
    ctx.print(format!("Answer: {answer}"));
    Ok(())
}

fn invalid(ctx: &mut Context, args: &[String]) -> CommandResult {
    no_args(args)?;
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

    let mut name = args.join(" ");
    let mut address = String::new();
    loop {
        let Outcome::Submitted(fields) = ctx.show(delivery_island(&name, &address))? else {
            ctx.print("Order cancelled.");
            return Ok(());
        };
        name = fields.text("name").trim().to_owned();
        address = fields.text("address").trim().to_owned();
        if !name.is_empty() && !address.is_empty() {
            break;
        }
        ctx.print("Please fill in both your name and address.");
    }

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

fn delivery_island(name: &str, address: &str) -> IslandBuilder {
    let mut builder = IslandBuilder::new(column([
        label("Step 2 of 2: delivery"),
        text_field("name", "Name"),
        text_field("address", "Address"),
        row([submit_button("Order"), cancel_button("Cancel")]),
    ]));
    builder.set_text("name", name);
    builder.set_text("address", address);
    builder
}

fn price(cents: u32) -> String {
    format!("€{}.{:02}", cents / 100, cents % 100)
}
