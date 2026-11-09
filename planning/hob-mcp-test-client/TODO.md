# hob-mcp-test-client: TODO

Decisions are in DECISIONS.md here.

- Next: resources, prompts, and showing server notifications (logging,
  progress, list changes; the tool list isn't refreshed on changes yet).
- `call` doesn't support the server asking for more input (MRTR's
  `input_required`) or making the call a task; both fail with a message.
- `call`'s form: nested objects are entered as JSON, not as fields of
  their own, and there's no picker for choices (`enum`).
- No timeouts or cancellation: a server that doesn't answer blocks the
  command until `disconnect` closes the connection.
- OAuth isn't used yet (the `http` feature includes rmcp's `auth`): a
  server that needs it fails to connect.
- rmcp's errors are long (e.g. a refused HTTP connection names the
  transport's full type, and rmcp also logs its own error notice). Showing
  just the cause chain would be shorter.
- The tools table breaks when descriptions are long (the name column gets
  squeezed under the text): the kettle-ui-iced layout bug in ../BUGS.md.
- Elicitation and sampling requests arrive in rmcp's handler, not on a
  command's thread; showing them as islands needs a bridge (e.g. a channel
  to a long-running command).
