//! Demo islands, to try out kettle-ui. Independent of any GUI toolkit.

use std::thread;
use std::time::Duration;

use kettle_ui::widget::*;
use kettle_ui::{Fields, IslandBuilder, Value};

const FRUITS: &[&str] = &[
    "apple",
    "apricot",
    "banana",
    "blackberry",
    "blueberry",
    "cherry",
    "grape",
    "grapefruit",
    "lemon",
    "lime",
    "mango",
    "melon",
    "orange",
    "peach",
    "pear",
    "pineapple",
    "plum",
    "raspberry",
    "strawberry",
    "watermelon",
];

/// A demo island.
#[derive(Clone, Copy, Debug)]
pub enum Demo {
    /// A simple form: a name and a checkbox.
    Contact,
    /// Search-as-you-type over a list of fruit, with suggestions.
    FruitSearch,
}

impl Demo {
    /// Every demo, in toolbar order.
    pub const ALL: [Demo; 2] = [Demo::Contact, Demo::FruitSearch];

    /// The name shown in the toolbar and on the tab.
    pub fn title(self) -> &'static str {
        match self {
            Demo::Contact => "Contact",
            Demo::FruitSearch => "Fruit search",
        }
    }

    /// Builds a new instance of the island.
    pub fn builder(self) -> IslandBuilder {
        match self {
            Demo::Contact => contact(),
            Demo::FruitSearch => fruit_search(),
        }
    }

    /// Describes what the user submitted.
    pub fn summary(self, fields: &Fields) -> String {
        match self {
            Demo::Contact => format!(
                "name {:?}, subscribe {}, message {:?}",
                fields.text("name"),
                fields.checked("subscribe"),
                fields.text("message")
            ),
            Demo::FruitSearch => format!("fruit {:?}", fields.text("fruit")),
        }
    }
}

fn contact() -> IslandBuilder {
    let mut builder = IslandBuilder::new(column([
        label("Name"),
        text_field("name", "Your name"),
        label(state("greeting")),
        checkbox("subscribe", "Subscribe to updates"),
        label("Message"),
        text_area("message", "Anything you'd like to tell us"),
        row([submit_button("Submit"), cancel_button("Cancel")]),
    ]));
    builder.derive("greeting", |f| match f.text("name") {
        "" => "Hello!".into(),
        name => format!("Hello, {name}!").into(),
    });
    builder
}

fn fruit_search() -> IslandBuilder {
    let mut builder = IslandBuilder::new(column([
        label(state("status")),
        progress("progress"),
        label("Favourite fruit"),
        text_field("fruit", "Start typing…"),
        suggestions("matches", "fruit"),
        when("no_matches", label("No matching fruit")),
        row([submit_button("Choose"), cancel_button("Cancel")]),
    ]));

    // Pretend this search is slow, to show that typing stays responsive.
    builder.derive("matches", |f| {
        let query = f.text("fruit").to_lowercase();
        if query.is_empty() {
            return Vec::new().into();
        }
        thread::sleep(Duration::from_millis(300));
        FRUITS
            .iter()
            .filter(|fruit| fruit.contains(&query))
            .take(5)
            .map(|fruit| fruit.to_string())
            .collect::<Vec<_>>()
            .into()
    });

    // Waits for "matches" to be up to date, so it never disagrees with it.
    builder.derive_with_state("no_matches", &["matches"], |i| {
        let none = matches!(i.state("matches"), Some(Value::List(m)) if m.is_empty());
        (none && !i.text("fruit").is_empty()).into()
    });

    // Pretend the fruit list is loaded in the background.
    builder.set_state("status", "Loading fruit…");
    builder.set_state("progress", 0.0);
    let handle = builder.handle();
    thread::spawn(move || {
        for i in 1..=50 {
            thread::sleep(Duration::from_millis(100));
            handle.set("progress", i as f64 / 50.0);
        }
        handle.set("status", "Fruit loaded");
    });

    builder
}
