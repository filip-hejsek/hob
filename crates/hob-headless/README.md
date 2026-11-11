# hob-headless

A frontend for [hob](https://github.com/filip-hejsek/hob) without a window: it runs commands for a
client program, which talks to it with JSON-RPC over stdin and stdout. The
client acts on islands like a user would (types text, presses buttons).
The `protocol` module has the messages, and `client` a client for them.

> **Most of the code in this project was written by AI** (Claude), with a
> person directing the design. It's an early prototype: expect bugs, and
> APIs that change between releases.

Like [hob-gui](https://crates.io/crates/hob-gui), it doesn't know about
specific commands: the application registers them on a
[hob-core](https://crates.io/crates/hob-core) `Interpreter`.

```rust,no_run
use hob_core::Interpreter;

let mut interpreter = Interpreter::named("my-app");
hob_core::builtins::register(&mut interpreter);
hob_headless::serve(interpreter, std::io::stdin(), std::io::stdout()).unwrap();
```

[hob-mcp](https://crates.io/crates/hob-mcp) uses it to let an AI agent use
hob.

## License

Copyright 2026 the authors listed in AUTHORS.

Licensed under either of the Apache License, Version 2.0, or the MIT
license, at your option.
