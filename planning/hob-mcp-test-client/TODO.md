# hob-mcp-test-client: TODO

Decisions are in DECISIONS.md here.

- Only a skeleton so far: the GUI with hob's builtins, and a tokio
  runtime. Plan: commands to connect to a server, list its tools, and call
  one through an island built from the tool's input schema.
- hob-gui hardcodes hob's identity: the window title "hob", the
  application ID (so desktops group it with hob), and the "hob:" prefix of
  log records on stderr. `exit` says "Exit hob". hob_gui::run could take
  them as options.
- Elicitation and sampling requests arrive in rmcp's handler, not on a
  command's thread; showing them as islands needs a bridge (e.g. a channel
  to a long-running command).
