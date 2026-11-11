# kettle-ui

A deliberately small UI framework for *islands*: form-like pieces of UI
embedded in a host application, which owns the window. It doesn't depend
on any GUI toolkit; a backend shows the island.

> **Most of the code in this project was written by AI** (Claude), with a
> person directing the design. It's an early prototype: expect bugs, and
> APIs that change between releases.

An island's widget tree is fixed once built. Fields have values, which the
host gets when the user submits; labels, lists and other widgets can show
*named state*, set from outside or derived from the fields as the user
types (computed on a background thread, so it may be slow, like a
search). Validators check the fields before submitting.

```rust
use kettle_ui::IslandBuilder;
use kettle_ui::widget::*;

let mut builder = IslandBuilder::new(column([
    text_field("name", "Your name"),
    label(state("greeting")),
    checkbox("subscribe", "Subscribe to updates"),
    label(state("error")),
    row([submit_button("OK"), cancel_button("Cancel")]),
]));
// Updated as the user types.
builder.derive("greeting", |f| match f.text("name") {
    "" => "Hello!".into(),
    name => format!("Hello, {name}!").into(),
});
// Checked when the user presses OK; shown only from then on.
builder.validate("error", |f| {
    f.text("name").is_empty().then(|| "Please enter your name.".to_owned())
});
```

Backends:

- [kettle-ui-iced](https://crates.io/crates/kettle-ui-iced) shows islands
  in an [iced](https://iced.rs) application.
- [kettle-ui-headless](https://crates.io/crates/kettle-ui-headless) drives
  them without a GUI, for tests and agents. It shows the island above as:

  ```text
  column
    text_field name: "" (placeholder "Your name")
    label "Hello!"
    checkbox subscribe: [ ] "Subscribe to updates"
    label ""
    row
      button "OK" (submit)
      button "Cancel" (cancel)
  ```

In [hob](https://github.com/filip-hejsek/hob), a command shows an island with `Context::show` and
waits for the user to submit it, as a terminal program waits for a line.

Images (`image` widgets) are compressed as JPEG XL with
[kettle-jxl](https://crates.io/crates/kettle-jxl), so building needs the
system libjxl (0.8 or newer).

## License

Copyright 2026 the authors listed in AUTHORS.

Licensed under either of the Apache License, Version 2.0, or the MIT
license, at your option.
