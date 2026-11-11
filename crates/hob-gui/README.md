# hob-gui

The shell of [hob](https://github.com/filip-hejsek/hob): an [iced](https://iced.rs) window with a
terminal-like scrollback and a command prompt. Each command gets a block in
the scrollback, with what it prints and the islands it shows; an island
freezes in place once submitted. The session can be saved and restored.

> **Most of the code in this project was written by AI** (Claude), with a
> person directing the design. It's an early prototype: expect bugs, and
> APIs that change between releases.

It doesn't know about specific commands: the application registers them
on a [hob-core](https://crates.io/crates/hob-core) `Interpreter`.

```rust,no_run
use hob_core::Interpreter;
use hob_gui::GuiConfig;

let mut interpreter = Interpreter::named("my-app");
hob_core::builtins::register(&mut interpreter);
let config = GuiConfig {
    title: "My app".into(),
    app_id: "org.example.MyApp".into(),
    command_placeholder: "Type a command, or help".into(),
    log_prefix: "my-app".into(),
    startup: Vec::new(),
};
hob_gui::run(interpreter, None, config).unwrap();
```

hob itself builds against a fork of iced 0.14 that fixes startup
notification (focus and the launcher's busy cursor when started from a
desktop file, iced#3317). The crates.io release uses stock iced, which
doesn't have that fix.

## License

Copyright 2026 the authors listed in AUTHORS.

Licensed under either of the Apache License, Version 2.0, or the MIT
license, at your option.
