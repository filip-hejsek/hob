# hob

A desktop GUI application in Rust, built on [iced](https://iced.rs).

Programming simple interactions in a terminal is easy: print a question, read
a response. One goal of this project is to make simple GUI interactions just as
easy.

The user runs commands from a prompt. A command talks to the user like a
terminal program does: it prints messages, and instead of reading a line, it
shows a piece of UI and waits for the user to submit it.

The project is split into these parts:

- **hob-core** — the command interpreter. It is toolkit-independent.
- **hob-gui** — the shell: owns the main window, and shows a terminal-like
  scrollback of commands and their islands, with a command prompt.
- **kettle-ui** — a deliberately small UI framework for *islands*: pieces of
  UI embedded in the shell. It is toolkit-independent; **kettle-ui-iced**
  renders it with iced, and **kettle-ui-headless** drives it without a GUI,
  for tests and (later) agents.
- **hob-demo** — demo commands.
- **hob** — the application: registers the commands and starts the shell.

Run it with:

```sh
cargo run
```

The history and scrollback are restored on the next start. They're saved in
the platform's state directory (e.g. `~/.local/state/hob/session.json`); set
`HOB_SESSION` to use another file.

Errors and warnings are shown in the scrollback; the `messages` command lists
all messages. Set `HOB_LOG=debug` to also show debug messages.

Early prototype; expect everything to change.

## License

Copyright 2026 Filip Hejsek

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
