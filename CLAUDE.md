# CLAUDE.md

See README.md for what the project is. Decisions already made, with
rejected alternatives, are in planning/DECISIONS.md; plans and open
questions are in planning/TODO.md. Check both before proposing design
changes, and keep them up to date: when something is done, remove it from
TODO.md and record the decisions it involved in DECISIONS.md.

When iced lacks something we need, add it to planning/ICED_WISHLIST.md,
with the workaround used.

## Architecture rules

- `kettle-ui` must not depend on any GUI toolkit. Anything iced-specific
  belongs in `kettle-ui-iced`.
- `kettle-ui-demo` uses only `kettle-ui`, not iced.
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
- `hob-demo` holds demo commands. Like `hob-core`, it must not depend on
  any GUI toolkit.
- kettle-ui is deliberately small: the widget tree is fixed once built, and
  spacing is fixed and styling options are minimal (see
  planning/DECISIONS.md). Don't add options without a need.

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
