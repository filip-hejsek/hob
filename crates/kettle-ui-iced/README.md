# kettle-ui-iced

The [iced](https://iced.rs) backend for
[kettle-ui](https://crates.io/crates/kettle-ui) islands: an `Island` is
embedded in an iced application, which drives it with `update` and
`view`. A frozen island (e.g. a saved result) is shown with `view_frozen`.

> **Most of the code in this project was written by AI** (Claude), with a
> person directing the design. It's an early prototype: expect bugs, and
> APIs that change between releases.

It also has the `kettle-render` tool, which renders an island saved as
JSON to a PNG, without a window:

```sh
kettle-render [--frozen] [--width N] [--theme NAME] island.json island.png
```

## License

Copyright 2026 the authors listed in AUTHORS.

Licensed under either of the Apache License, Version 2.0, or the MIT
license, at your option.
