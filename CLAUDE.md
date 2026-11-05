# CLAUDE.md

See README.md for what the project is. Plans, open questions and decisions
already made are in TODO.md: check it before proposing design changes, and
keep it up to date.

## Architecture rules

- `kettle-ui` must not depend on any GUI toolkit. Anything iced-specific
  belongs in `kettle-ui-iced`.
- `kettle-ui-demo` uses only `kettle-ui`, not iced.
- `hob-gui` (the shell) is plain iced; it hosts islands through
  `kettle-ui-iced`.
- kettle-ui is deliberately small: the widget tree is fixed once built, and
  spacing is fixed and styling options are minimal (see TODO.md). Don't add
  options without a need.

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
