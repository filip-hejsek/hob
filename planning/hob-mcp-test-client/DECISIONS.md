# hob-mcp-test-client: decisions

A subproject: plans are in TODO.md here. Decisions for the rest of hob are
in ../DECISIONS.md.

## Purpose

- A standalone program that lets a person use an MCP server the way an
  agent would. Hob provides the GUI: it depends on hob-gui and registers
  its own commands, not hob's (only the generic `hob_core::builtins`:
  `clear`, `exit`, `messages`, ..., but not the shell command `!`).
- Its session is saved apart from hob's (`HOB_MCP_TEST_CLIENT_SESSION`,
  or `hob-mcp-test-client/session.json` in the state directory).
- Its interpreter is named `hob-mcp-test-client` (so `exit` says "Exit
  hob-mcp-test-client"), and it passes hob-gui its own configuration: the
  title and stderr prefix `hob-mcp-test-client`, and the application ID
  `io.github.filip_hejsek.hob_mcp_test_client`, so desktops don't group its
  window with hob's. There's no desktop file for it yet.

## Build

- It lives in `tools/hob-mcp-test-client`, outside `crates/`. It's a
  workspace member (sharing the lockfile, the iced patch and `target/`) but
  not a default member, so plain `cargo build`, `test` and `clippy` skip it;
  build it with `-p hob-mcp-test-client`. Developing the other crates
  doesn't need to build or test it, and changes to them may break it for a
  while. Rejected: a separate workspace (a second copy of the iced patch,
  and of iced in its own `target/`).
- The commands are in the crate's library (`register`, `Client`), and the
  program only sets up the GUI, so tests can run them. The test program
  is also the MCP server they connect to (`harness = false`, started with
  `--server` and other modes), as in hob-mcp's tests.

## MCP

- MCP comes from the official Rust SDK (`rmcp`). A client that should work
  with many servers needs much more of MCP than hob-mcp does (transports,
  auth, version negotiation, pagination, cancellation, server requests
  such as elicitation, notifications), and rmcp has it all. Its tokio
  dependency is fine in a separate program: hob and hob-mcp don't change.
  It also checks hob-mcp's hand-written server against an independent
  implementation. Rejected: implementing the client by hand, like
  hob-mcp's server.
- stdio is always built. Streamable HTTP and OAuth are behind a cargo
  feature (`http`), since they add about 90 crates (reqwest, hyper,
  oauth2, TLS). It also enables rmcp's `reqwest` feature: the HTTP
  transport alone has no TLS.
- Commands block on their own threads, so they reach rmcp's async API
  through a tokio runtime the program holds (`Handle::block_on`); iced
  doesn't need tokio.
- The client introduces itself as `hob-mcp-test-client` (rmcp's default
  is `rmcp`), with no capabilities yet.

## Commands

- `connect SERVER [ARG...]` starts the program SERVER in the working
  directory and talks to it over stdio; with `http`, SERVER may be an
  `http(s)://` URL instead. It prints the server's name and version, the
  protocol version, what it offers (by capability) and its instructions.
- One connection at a time. A new `connect` replaces the old connection
  once it has succeeded, so a failed one leaves the old connection.
  `disconnect` closes it. Rejected: refusing to connect while connected
  (an extra step when switching servers).
- A server's stderr is logged at info (target
  `hob_mcp_test_client::server`), so `messages` shows it; servers log
  routinely to stderr, so it doesn't belong in the scrollback. If
  connecting fails, the last 50 lines (most likely why) are printed in
  `connect`'s block.
- When a request finds the connection closed (e.g. the server exited), the
  connection is forgotten and the command says so; rmcp doesn't otherwise
  report it.
- The program closes the connection before it exits: rmcp kills a server
  process it drops by spawning a task, which may not run while the runtime
  shuts down.
- `tools` lists the tools as a table (name, and title or first line of the
  description), like `help`; `tools TOOL` shows one in full: description,
  annotation hints, and the schemas as pretty JSON. JSON keys are sorted
  (serde_json without `preserve_order`; enabling it here would change other
  crates' JSON when built together). Tool names are completed from the
  list last fetched (by `connect` or `tools`), since completion can't wait
  for the server.
- `call TOOL` asks for the arguments through a form built from the tool's
  input schema: one field per property, alphabetically (serde_json has
  already lost the schema's order). Strings and numbers get a text field,
  booleans a checkbox, anything else (objects, arrays, unions, no type) a
  text area holding JSON. A type with `null` is entered as the other type.
  Choices (`enum`) and defaults are shown as the placeholder. The form
  shows the arguments it makes as JSON, live, and what's wrong with them
  (e.g. "count: not an integer"); submitting a form with problems shows it
  again. A tool without parameters is called without asking.
- In the form, an empty field is left out (so the tool uses its default),
  and a checkbox is sent only if it's required or differs from the default
  (false if none). Required fields aren't enforced: the server checks the
  arguments, as it would an agent's. Anything the form can't express (an
  empty string, `null`, arguments outside the schema) goes through "Edit as
  JSON", which turns the arguments so far into a JSON text area, or
  `call TOOL JSON`, which sends the JSON object given without asking.
- The result is shown in order: text as it is, images in an island,
  embedded resources and links described (with text resources' text), then
  the structured content as pretty JSON. A result marked as an error makes
  the command fail after showing it.
