# hob

A desktop GUI application in Rust, built on [iced](https://iced.rs).

Programming simple interactions in a terminal is easy: print a question, read
a response. One goal of this project is to make simple GUI interactions just as
easy.

The UI is split into two parts:

- **hob-gui** — the shell: owns the main window and hosts islands.
- **kettle-ui** — a deliberately small UI framework for *islands*: pieces of
  UI embedded in the shell. It is toolkit-independent; **kettle-ui-iced**
  renders it with iced, and **kettle-ui-headless** drives it without a GUI,
  for tests and (later) agents.

Run it with:

```sh
cargo run
```

Early prototype; expect everything to change.

## License

Copyright 2026 Filip Hejsek

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
