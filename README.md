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
- **kettle-jxl** — minimal bindings to libjxl, for compressing kettle-ui
  images as JPEG XL.
- **hob-demo** — demo commands.
- **hob-fs** — filesystem commands: `pwd`, `cd`, `ls`, `cat`.
- **hob-git** — Git commands, using gitoxide rather than the `git` program:
  `git status`, `git log`, `git branch`, `git diff`.
- **hob-notes** — notes and TODO items, kept in SQLite: `note`, `title`,
  `todo`, `done`, `notes`.
- **hob-mcp-test-client** (in `tools/`) — an MCP client for people: try
  an MCP server the way an agent would, with hob as the GUI. Built only
  on request (`cargo build -p hob-mcp-test-client`, `--features http` for
  remote servers).
- **hob** — the application: registers the commands and starts the shell
  (or, with `--headless`, the headless frontend; with `--preview`, a
  read-only view of a saved session).

Building needs libjxl (0.8 or newer, found with pkg-config), e.g. the
`libjxl` package on Arch or `libjxl-dev` on Debian and Ubuntu.

Run it with:

```sh
cargo run
```

To add hob to the desktop's application menu, install it and its desktop
file:

```sh
cargo install --path crates/hob
desktop-file-install --dir ~/.local/share/applications data/io.github.filip_hejsek.hob.desktop
```

The history and scrollback are restored on the next start. They're saved in
the platform's state directory (e.g. `~/.local/state/hob/session.json`); set
`HOB_SESSION` to use another file. Old output is dropped once the file
gets big: past 4 MiB of text (`HOB_SESSION_SIZE`), or 16 MiB of images
(`HOB_SESSION_IMAGES`), after which older images show only their alt text.
Sizes are in bytes, or with a `K`, `M` or `G` suffix; small values help
when trying the trimming out. The history keeps 10,000 lines.

Notes are kept in the platform's data directory (e.g.
`~/.local/share/hob/notes.sqlite`); set `HOB_NOTES` to use another file,
e.g. when trying hob out (also through hob-mcp), so real notes aren't
touched. `note` takes a note (the arguments, or it asks), and `title`
names the last one (or `-n ID`). `todo` offers a few things to do now:
TODO notes (whose title or text starts with the word "todo") to work on,
and unnamed notes to name. Picking a TODO note makes it active, until
`done` (which also takes an ID). `notes` lists the notes; `notes show`,
`edit`, `delete` and `reopen` act on one, given as `3` or `#3`.

`hob --preview [FILE]` shows a saved session (by default, the current one)
read-only: nothing runs, and the file isn't changed.

Each program prints its version and the Git commit it was built from with
`--version`.

Arguments can be quoted like in a shell (`cat 'my file'`, `"my file"` or
`my\ file`), and `~` is the home directory.

A line starting with `!` runs a shell command (with `sh -c`), e.g.
`!git status`. Its output is shown as it comes; it can't read input yet.

Errors and warnings are shown in the scrollback (warnings from libraries only
on stderr); the `messages` command lists all messages. Set `HOB_LOG=debug` to
also show debug messages.

Images are saved once each, however often they're shown, compressed as
JPEG XL where that's smaller. Commands can instead mark an image as linked,
so only its file's path is saved and the file is shown again if unchanged,
or as ephemeral, so it isn't saved at all (try `view -l` and `view -e`).

To try the headless frontend, run the demo client, which starts
`hob --headless` from the same directory:

```sh
cargo build && target/debug/hob-cli
```

To build hob without the window and iced (only `--headless` works then),
turn off its `gui` feature: `cargo build -p hob --no-default-features`.

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

This repository's `.mcp.json` sets that up for Claude Code, with a release
build kept apart from work in progress; install it with
`scripts/install-hob-mcp`.

The headless frontend sends images without the paths of the files they came
from, unless `HOB_IMAGE_PATHS=1` is set.

To see an island without starting the GUI, render it to a PNG with
`kettle-render` (`--frozen` gives the disabled look of the scrollback,
`--width N` sets the width, `--theme NAME` picks one of iced's built-in
themes, e.g. `dark`; the default is `light`). It reads a frozen island as JSON: e.g. from the
session file's scrollback, or from the `contents` of a hob-headless `island`
notification. For example, to render the last island in the session
(with its images, which the session keeps in a table of their own):

```sh
jq '.images as $images
    | [.scrollback[].block?.entries[]? | .island // empty] | last
    | .state |= map_values(if type == "object" then . + ($images[.key] // {}) else . end)' \
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
