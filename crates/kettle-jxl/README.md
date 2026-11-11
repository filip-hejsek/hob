# kettle-jxl

Minimal bindings to the system [libjxl](https://github.com/libjxl/libjxl),
the JPEG XL reference library, under a permissive license (MIT or
Apache-2.0). The other bindings on crates.io (`jpegxl-sys`, `jpegxl-rs`)
are GPL.

> **Most of the code in this project was written by AI** (Claude), with a
> person directing the design. It's an early prototype: expect bugs, and
> APIs that change between releases.

The bindings are written by hand, for just what
[kettle-ui](https://github.com/filip-hejsek/hob) needs:

- lossless encoding of 8- or 16-bit gray, gray + alpha, RGB or RGBA pixels,
  with an optional ICC profile (`encode_lossless`);
- lossless recompression of JPEG files, and reconstructing the original
  file exactly (`recompress_jpeg`, `reconstruct_jpeg`);
- decoding to 8-bit RGBA (`decode_rgba`), and reading the size
  (`dimensions`).

```rust,no_run
fn shrink(jpeg: &[u8]) -> Result<Vec<u8>, kettle_jxl::Error> {
    let jxl = kettle_jxl::recompress_jpeg(jpeg)?;
    assert_eq!(kettle_jxl::reconstruct_jpeg(&jxl)?.as_deref(), Some(jpeg));
    Ok(jxl)
}
```

Building needs libjxl 0.8 or newer, found with pkg-config: e.g. the
`libjxl` package on Arch, or `libjxl-dev` on Debian and Ubuntu. Nothing
is vendored or built from source.

## License

Copyright 2026 the authors listed in AUTHORS.

Licensed under either of the Apache License, Version 2.0, or the MIT
license, at your option.
