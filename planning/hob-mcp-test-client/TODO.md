# hob-mcp-test-client: TODO

Decisions are in DECISIONS.md here.

- Progress is shown as lines. A progress bar would need an island that
  its command can close when the request ends; `Context::show` waits for
  the user.
- MCP 2026-07-28 (no `initialize`; `subscriptions/listen` for list
  changes and resource updates) isn't supported: see DECISIONS.md.
- `call`, `read` and `prompt` don't support the server asking for more
  input (MRTR's `input_required`), and `call` doesn't follow a call made a
  task; both fail with a message.
- Completions (`completion/complete`) aren't used: they could suggest
  values in the prompt form, and fill in a resource template's variables
  (a form for templates would beat typing the URI).
- Binary resources can't be saved to a file.
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
