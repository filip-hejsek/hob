# hob-demo

Demo commands for [hob](https://github.com/filip-hejsek/hob). Between them, they use everything a
command can do through [hob-core](https://crates.io/crates/hob-core):
printing, logging, islands with derived state and validation, background
updates, images, and arguments.

> **Most of the code in this project was written by AI** (Claude), with a
> person directing the design. It's an early prototype: expect bugs, and
> APIs that change between releases.

```rust
let mut interpreter = hob_core::Interpreter::named("my-app");
hob_demo::register(&mut interpreter);
```

## License

Copyright 2026 the authors listed in AUTHORS.

Licensed under either of the Apache License, Version 2.0, or the MIT
license, at your option.
