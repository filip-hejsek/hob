# CLAUDE.md

See README.md for what the project is. Decisions already made, with
rejected alternatives, are in planning/DECISIONS.md; plans and open
questions are in planning/TODO.md, and known bugs in planning/BUGS.md.
Check them before proposing design changes, and keep them up to date: when
something is done or fixed, remove it from TODO.md or BUGS.md and record
the decisions it involved in DECISIONS.md.

When iced lacks something we need, add it to planning/ICED_WISHLIST.md,
with the workaround used.

## Architecture rules

- `kettle-ui` must not depend on any GUI toolkit. Anything iced-specific
  belongs in `kettle-ui-iced`. That crate also holds the `kettle-render`
  tool, which renders island JSON to a PNG without a window.
- `kettle-ui-demo` uses only `kettle-ui`, not iced.
- `kettle-jxl` holds hand-written bindings to the system libjxl, used by
  `kettle-ui` for images. It's the only crate with `unsafe` code; keep it
  small and keep the structs in step with libjxl's headers.
- `kettle-ui-headless` is a backend without a GUI, for tests and agents. It
  must not depend on iced either.
- `hob-core` (the command interpreter) must not depend on any GUI toolkit;
  it uses only `kettle-ui`.
- `hob-gui` (the shell) is plain iced; it hosts islands through
  `kettle-ui-iced`. It doesn't know about specific commands; the `hob`
  crate registers them.
- `hob-headless` (the frontend without a window) must not depend on any
  GUI toolkit either. Like `hob-gui`, it doesn't know about specific
  commands. Clients use its `protocol` module.
- `hob-cli` is a demo client for `hob-headless`, and `hob-mcp` an MCP
  server that bridges to it; no GUI toolkit in either.
- `hob-demo` holds demo commands, `hob-fs` filesystem commands, and
  `hob-git` Git commands. Like `hob-core`, they must not depend on any GUI
  toolkit.
- kettle-ui is deliberately small: the widget tree is fixed once built, and
  spacing is fixed and styling options are minimal (see
  planning/DECISIONS.md). Don't add options without a need.

## Trying hob

The `hob` MCP server (hob-mcp, configured in `.mcp.json`) is mainly for
trying commands while developing them: start hob with its `start` tool
(e.g. `["target/debug/hob", "--headless"]`; rebuild first, then start
again after changes).

It runs `.bin/hob-mcp`, a release build kept apart from work in progress and
`cargo clean`. If it's missing or out of date, run `scripts/install-hob-mcp`,
then ask the user to reconnect it (`/mcp reconnect hob`).

Don't use it:

- for anything scripted or repeated: its tools can only be called one at
  a time, by hand. Write a test (hob-mcp's tests show how to drive hob),
  or pipe lines to `target/debug/hob-cli`. That works only for commands
  without islands: it doesn't wait for an island before running the next
  line, so `/type` and `/press` fail with "no live island".
- when changing the headless protocol or hob-mcp itself: the installed
  server won't have your changes. Test them with their tests or hob-cli,
  and install the server again once they're committed.

Some of a command's behavior can't be seen through it. Results sum up
what happened since the last call, so intermediate states are often lost:
an island that changed several times is shown only as it ends up (e.g.
"island #14 changed (30 times)"). Check such behavior with a test.

## Checks

Before committing, all of these must pass without warnings:

```sh
cargo fmt --check
cargo clippy --all-targets
cargo test
RUSTDOCFLAGS=-Dwarnings cargo doc --no-deps
```

The minimum supported Rust version is 1.90 (`rust-version` in Cargo.toml).
Don't use language or std features newer than that.

## Style

- Docs: keep them short, since the code is still changing. Every public
  item needs at least a one-line doc (`missing_docs` is on).
- Commits: short imperative subject line; add a body only when the why
  isn't obvious.
- Commit finished work without asking, once the checks pass; a commit can
  be amended if more changes are needed. Don't push, unless you are in
  a cloud session.
