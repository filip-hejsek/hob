# iced wishlist

Features we'd like iced to have, found while building hob. Each says what
we needed, what's missing (as of iced 0.14), and how we work around it.
Candidates for upstream issues or PRs.

## Reading a text input's cursor and selection

**Needed for:** Tab completion at the cursor in the prompt (hob-gui).

The app can move the cursor and select text (`operation::move_cursor_to`,
`select_range`), but can't read where the cursor or selection is: the
`TextInput` operation trait only exposes the text.

**Workaround:** completion works only at the end of the line.

## Key handlers on a text input

**Needed for:** history (Up/Down) and completion (Tab) in the prompt.

`text_input` has `on_input` and `on_submit`, but no way to react to other
keys pressed while it's focused.

**Workaround:** a global keyboard subscription, plus an
`operation::is_focused` round trip for each key press to check that the
prompt is focused.

## An element that sends no messages

**Needed for:** showing frozen islands (kettle-ui-iced `view_frozen`), which
have no interaction, inside any host's message type.

There's no message type for "never sends", so a non-interactive element
must still be built for some concrete message type.

**Workaround:** build it for our own message type and
`.map(|_| unreachable!())`.

## Pixels of a test snapshot

**Needed for:** rendering an island to a PNG (kettle-ui-iced `render_png`,
the `kettle-render` tool), and snapshot tests of it.

`iced_test`'s `Snapshot` doesn't expose its pixels: it can only be compared
with a PNG file, or written to one if the file is missing. The `Simulator`
also has a fixed size (1024×768) and scale (2×), and tries wgpu first.

**Workaround:** lay out and draw the element ourselves with a `Headless`
renderer (tiny-skia), and call its `screenshot`.

## Widget status without events

**Needed for:** drawing widgets offscreen, without a runtime (`render_png`).

Widgets such as `button` work out their status (active, disabled, ...) only
in `update`, on a `RedrawRequested` event, and draw as disabled until then.
So drawing a freshly laid-out element shows every button disabled.

**Workaround:** send one `RedrawRequested` event through `update` before
drawing.
