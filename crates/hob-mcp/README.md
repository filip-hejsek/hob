# hob-mcp

An [MCP](https://modelcontextprotocol.io) server (over stdio) that lets an
AI agent use [hob](https://github.com/filip-hejsek/hob), through its headless frontend
([hob-headless](https://crates.io/crates/hob-headless)). The agent runs
commands, and fills in and submits their islands, as a user would.

> **Most of the code in this project was written by AI** (Claude), with a
> person directing the design. It's an early prototype: expect bugs, and
> APIs that change between releases.

hob-mcp is self-contained: it starts hob as another program, so it works
with any hob build (or another program that serves the same protocol).
It starts:

- `hob --headless` from its own directory, by default;
- the command given as arguments, e.g. `hob-mcp /path/to/hob --headless`;
- nothing, with `--agent-spawns`: the agent starts hob with the `start`
  tool, giving the command. This suits an agent working on hob, which
  rebuilds it.

If hob exits, so does hob-mcp, unless `--allow-restart` (implied by
`--agent-spawns`) lets the agent start it again.

For example, for Claude Code:

```sh
cargo install hob-mcp
claude mcp add hob -- hob-mcp /path/to/hob --headless
```

hob itself isn't on crates.io yet: build it from the
[repository](https://github.com/filip-hejsek/hob).

## License

Copyright 2026 the authors listed in AUTHORS.

Licensed under either of the Apache License, Version 2.0, or the MIT
license, at your option.
