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
- **hob-headless** — a frontend without a window, for client programs that
  talk to it with JSON-RPC over stdin and stdout.
- **hob-cli** — a demo client for hob-headless: an interactive command line.
- **hob-mcp** — an MCP server that lets an agent use hob, through
  hob-headless.
- **kettle-ui** — a deliberately small UI framework for *islands*: pieces of
  UI embedded in the shell. It is toolkit-independent; **kettle-ui-iced**
  renders it with iced, and **kettle-ui-headless** drives it without a GUI,
  for tests and (later) agents.
- **hob-demo** — demo commands.
- **hob-fs** — filesystem commands: `pwd`, `cd`, `ls`, `cat`.
- **hob** — the application: registers the commands and starts the shell
  (or, with `--headless`, the headless frontend).

Run it with:

```sh
cargo run
```

The history and scrollback are restored on the next start. They're saved in
the platform's state directory (e.g. `~/.local/state/hob/session.json`); set
`HOB_SESSION` to use another file.

Errors and warnings are shown in the scrollback (warnings from libraries only
on stderr); the `messages` command lists all messages. Set `HOB_LOG=debug` to
also show debug messages.

To try the headless frontend, run the demo client, which starts
`hob --headless` from the same directory:

```sh
cargo build && target/debug/hob-cli
```

To let an agent use hob, add `hob-mcp` as an MCP server (it also starts
`hob --headless` from its directory), e.g. for Claude Code:

```sh
cargo build && claude mcp add hob -- "$PWD/target/debug/hob-mcp"
```

If hob exits (e.g. it crashes), hob-mcp exits too. With `--allow-restart`,
the agent can instead start it again with the `start` tool, and close it
with `close`; `exit` then really exits hob. With `--agent-spawns` (which
implies `--allow-restart`), hob-mcp starts nothing; the agent starts hob
itself with `start`, giving the command (e.g. `target/debug/hob
--headless`). This suits an agent working on hob, which rebuilds it.

To see an island without starting the GUI, render it to a PNG with
`kettle-render` (`--frozen` gives the disabled look of the scrollback,
`--width N` sets the width). It reads a frozen island as JSON: e.g. from the
session file's scrollback, or from the `contents` of a hob-headless `island`
notification. For example, to render the last island in the session:

```sh
jq '[.scrollback[].block?.entries[]? | .island // empty] | last' \
    ~/.local/state/hob/session.json > island.json
cargo run -p kettle-ui-iced --bin kettle-render -- island.json island.png
```

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
