# hob-core

The command interpreter of [hob](https://github.com/filip-hejsek/hob), independent of any GUI toolkit.

> **Most of the code in this project was written by AI** (Claude), with a
> person directing the design. It's an early prototype: expect bugs, and
> APIs that change between releases.

Hob runs commands from a prompt, like a terminal. A command talks to the
user like a terminal program does: it prints messages, and instead of
reading a line, it shows a [kettle-ui](https://crates.io/crates/kettle-ui)
island (a small form) and waits for the user to submit it. Each command
runs on its own thread.

```rust
use hob_core::Interpreter;
use kettle_ui::widget::*;
use kettle_ui::{IslandBuilder, Outcome};

let mut interpreter = Interpreter::named("my-app");
hob_core::builtins::register(&mut interpreter);
interpreter.register("greet", "Greets you", |ctx, _args| {
    let builder = IslandBuilder::new(column([
        text_field("name", "Your name"),
        row([submit_button("OK"), cancel_button("Cancel")]),
    ]));
    if let Outcome::Submitted(fields) = ctx.show(builder)? {
        ctx.print(format!("Hello, {}!", fields.text("name")));
    }
    Ok(())
});
```

A frontend runs the commands and shows what they do:
[hob-gui](https://crates.io/crates/hob-gui) in a window, or
[hob-headless](https://crates.io/crates/hob-headless) for a client
program.

## License

Copyright 2026 the authors listed in AUTHORS.

Licensed under either of the Apache License, Version 2.0, or the MIT
license, at your option.
