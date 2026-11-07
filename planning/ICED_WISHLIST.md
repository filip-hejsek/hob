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

## Widget status without events

**Needed for:** drawing widgets offscreen, without a runtime (`render_png`).

Widgets such as `button` work out their status (active, disabled, ...) only
in `update`, on a `RedrawRequested` event, and draw as disabled until then.
So drawing a freshly laid-out element shows every button disabled.

**Workaround:** send one `RedrawRequested` event through `update` before
drawing.

## Subscriptions and more keys in `iced_test`'s `Emulator`

**Needed for:** end-to-end tests of the prompt's history (Up/Down) and
completion (Tab) in hob-gui.

The `Emulator` passes interactions only to the widgets, not to
subscriptions such as `event::listen_with` or `window::close_requests`.
Also, its `Key` has only Enter, Escape, Tab and Backspace, no arrows.

**Workaround:** history and completion are tested on `History` and
`Interpreter::complete`, and checked by hand in a real window (Xvfb).

## Waiting for an app to settle in `iced_test`

**Needed for:** end-to-end tests of hob-gui, where commands run on threads
and some wait for an island.

`Mode::Zen` waits for all tasks, which never happens while a command waits
for an island; `Mode::Immediate` doesn't wait at all, so even the boot task
(focusing the prompt) may not have run when `Ready` arrives. There's no
"wait until idle, ignoring long-running streams".

**Workaround:** a harness that retries each expectation until it holds or
times out, and a click on the prompt before typing into it.

## Choosing the renderer of `iced_test`'s `Emulator`

**Needed for:** fast, reproducible hob-gui tests on any machine.

`Simulator` reads `ICED_TEST_BACKEND`, but `Emulator` always tries wgpu
first, falling back to tiny-skia.

**Workaround:** none; the tests don't depend on pixels. In the container
there's no GPU, so they use tiny-skia.

## Exit in the test emulator

**Needed for:** testing the `exit` command in hob-gui's end-to-end tests.

`iced_test`'s `Emulator` ignores `iced::exit()` (a TODO in its source), so a
test can't tell that the app asked to exit.

**Workaround:** none in the tests; checked by hand under Xvfb.
