# hob-mcp-test-client: TODO

Decisions are in DECISIONS.md here.

- Not started. Plan: commands to connect to a server, list its tools, and
  call one through an island built from the tool's input schema.
- rmcp: pin the version (it breaks its API often). The `http` feature
  needs rmcp's `reqwest` feature too: the HTTP transport alone has no TLS.
- Elicitation and sampling requests arrive in rmcp's handler, not on a
  command's thread; showing them as islands needs a bridge (e.g. a channel
  to a long-running command).
