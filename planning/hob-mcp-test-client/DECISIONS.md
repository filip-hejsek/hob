# hob-mcp-test-client: decisions

A subproject: plans are in TODO.md here. Decisions for the rest of hob are
in ../DECISIONS.md.

## Purpose

- A standalone program that lets a person use an MCP server the way an
  agent would. Hob provides the GUI: it depends on hob-gui and registers
  its own commands, not the built-in ones.

## Build

- It lives in `tools/hob-mcp-test-client`, outside `crates/`. It's a
  workspace member (sharing the lockfile, the iced patch and `target/`) but
  not a default member, so plain `cargo build`, `test` and `clippy` skip it;
  build it with `-p hob-mcp-test-client`. Developing the other crates
  doesn't need to build or test it, and changes to them may break it for a
  while. Rejected: a separate workspace (a second copy of the iced patch,
  and of iced in its own `target/`).

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
  oauth2, TLS).
- Commands block on their own threads, so they reach rmcp's async API
  through a tokio runtime the program holds (`Handle::block_on`); iced
  doesn't need tokio.
