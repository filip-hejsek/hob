# kettle-ui-headless

A backend for [kettle-ui](https://crates.io/crates/kettle-ui) islands
without a GUI, for tests and for driving islands from a program (e.g. an
agent). An `Island` acts like a user would: it types into fields, toggles
checkboxes and presses buttons, by the ids and labels a user would see,
and shows the island as an outline.

> **Most of the code in this project was written by AI** (Claude), with a
> person directing the design. It's an early prototype: expect bugs, and
> APIs that change between releases.

```rust
use kettle_ui::widget::*;
use kettle_ui::{IslandBuilder, Outcome};
use kettle_ui_headless::Island;

let builder = IslandBuilder::new(column([
    text_field("name", "Your name"),
    submit_button("OK"),
]));
let mut island = Island::new(builder).unwrap();
island.type_text("name", "Ada").unwrap();
let Outcome::Submitted(fields) = island.press("OK").unwrap() else {
    panic!("not submitted");
};
assert_eq!(fields.text("name"), "Ada");
```

## License

Copyright 2026 the authors listed in AUTHORS.

Licensed under either of the Apache License, Version 2.0, or the MIT
license, at your option.
