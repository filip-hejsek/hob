# iced wishlist

Features we'd like iced to have, found while building hob. Each says what
we needed, what's missing (as of iced 0.14), and how we work around it.
Candidates for upstream issues or PRs.

## Reading a text input's cursor and selection

**Needed for:** Tab completion at the cursor in the prompt, and Ctrl+W,
Ctrl+U and Ctrl+K (hob-gui).

The app can move the cursor and select text (`operation::move_cursor_to`,
`select_range`), but can't read where the cursor or selection is: the
`TextInput` operation trait only exposes the text.

**Workaround:** completion works only at the end of the line; the
shortcuts wait for this.

## Which visual row a text editor's caret is on

**Needed for:** Up from the first row and Down from the last row of a
text area moving to the island's previous and next field.

`Content::cursor` gives the caret's logical line and column, but not which
wrapped row it's on, nor whether it's on the first or last one.

**Workaround:** none; Up and Down stay in a text area. (It could be found
by moving the caret with Home and DocumentStart, End and DocumentEnd, and
comparing, then moving it back, but that wasn't worth it.)

## Key handlers on a text input

**Needed for:** history (Up/Down) and completion (Tab) in the prompt;
Enter and Escape in the text fields of an island.

`text_input` has `on_input` and `on_submit`, but no way to react to other
keys pressed while it's focused.

**Workaround:** for the prompt, a global keyboard subscription, plus an
`operation::is_focused` round trip for each key press to check that the
prompt is focused. For islands, a wrapper widget that sees key events
before the island's widgets, and finds the focused field with an operation
on its children.

## A text editor that takes its value

**Needed for:** text areas in islands, whose value lives in the kettle-ui
model as a string, and in frozen islands, which have no editor state.

`text_editor` borrows a `Content` that the app must keep and update with
every `Action`, unlike `text_input`, which takes the value as a `&str` and
keeps its cursor in its widget state.

**Workaround:** a wrapper widget (kettle-ui-iced `TextArea`) that keeps the
`Content` in its own widget state and makes a temporary `text_editor` for
each call. It applies the editor's actions itself and forwards the rest of
the editor's shell requests. Since the editor is temporary, it can't keep
its status (hovered, focused) either, so the wrapper tracks it and passes
it in through the style.

## Selectable text

**Needed for:** copying command output in hob-gui.

iced's `text` and `rich_text` can't be selected.

**Workaround:** hob-gui's `SelectableText`, a wrapper around `text_editor`
(like kettle-ui-iced's `TextArea`) that drops edit actions, ignores the
wheel and editing keys, and focuses itself only while it has a selection.
A selection can't span several widgets.

## Primary selection in text widgets

**Needed for:** middle-click paste (X11 and Wayland): selecting text in
the prompt, a text field, a text area or the scrollback should make it the
primary selection, and middle-clicking a text field should paste it there.

iced's clipboard supports it (`clipboard::Kind::Primary`, implemented by
`window_clipboard` on X11 and Wayland), but `text_input` and `text_editor`
use only `Kind::Standard`: selecting doesn't write the primary selection,
and they ignore the middle button. Requested in iced#904 (open since 2021).
On iced master (2026-11) it's further off: the clipboard moved to
`arboard` and lost `Kind::Primary`; iced#3325 (open) would bring it back,
but again without widget support.

**Workaround:** none yet; middle-click paste doesn't work in hob-gui. Our
`text_editor` wrappers (`TextArea`, `SelectableText`) could add it: write
the selection to `Kind::Primary` when it changes, and on a middle press
move the cursor there and paste. For `text_input`s (the prompt, text
fields), whose selection we can't read, a wrapper could fake it: on a
middle press, send a left click and a Ctrl+V, giving the widget a
clipboard whose standard reads return the primary selection (and likewise
a Ctrl+C with writes redirected after the selection changes).

## Focusable buttons and checkboxes

**Needed for:** using islands from the keyboard.

Only `text_input` and `text_editor` can be focused; buttons, checkboxes
and the like can't, so focus operations skip them and they take no keys.

**Workaround:** checkboxes are wrapped in our own widget implementing
`Focusable`, with the island's wrapper handling their keys. An island
without fields can be focused as a whole (another wrapper implementing
`Focusable`), and its buttons have keys.

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

**Needed for:** end-to-end tests of the shell's key bindings in hob-gui:
history (Up/Down), completion (Tab), PageUp/PageDown, Ctrl+L; and of
Ctrl+Enter in kettle-ui-iced islands.

The `Emulator` passes interactions only to the widgets, not to
subscriptions such as `event::listen_with` or `window::close_requests`.
Also, its `Key` has only Enter, Escape, Tab and Backspace (no arrows or
PageUp/PageDown), and there are no modifiers.

**Workaround:** history and completion are tested on `History` and
`Interpreter::complete`, Ctrl+L by calling the shell's `update` directly
(or the emulator's `update`, with the shell's key message), and all of them
checked by hand in a real window (Xvfb).

## Finding a text editor's text in `iced_test`

**Needed for:** checking what was typed into a text area in kettle-ui-iced
tests.

`Expectation::Text` finds the text of `text` and `text_input` widgets, but
not of a `text_editor`.

**Workaround:** the tests submit the island and check the submitted value.

## Waiting for an app to settle in `iced_test`

**Needed for:** end-to-end tests of hob-gui, where commands run on threads
and some wait for an island.

`Mode::Zen` waits for all tasks, which never happens while a command waits
for an island; `Mode::Immediate` doesn't wait at all, so even the boot task
(focusing the prompt) may not have run when `Ready` arrives. There's no
"wait until idle, ignoring long-running streams".

**Workaround:** a harness that retries each expectation until it holds or
times out, a click on the prompt before typing into it, and waiting until
no actions came for 50 ms before typing after focus should have moved.

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

## A keyed column that matches any change

**Needed for:** keeping a live island's widget state (focus, cursor) in
hob-gui's scrollback when `clear` removes the finished blocks around it.

`keyed_column` matches children to their state by key only when one run of
children was added or removed; after other changes (e.g. children removed
both before and after a kept one), state goes to the wrong children.

**Workaround:** hob-gui's own `Keyed` widget, a wrapper around `column`
that reorders the children's state by key before diffing.

## JPEG XL images

**Needed for:** showing images, which kettle-ui keeps compressed as JPEG
XL where that's smaller.

iced's image widget decodes PNG, JPEG, GIF and WebP (through the `image`
crate), but not JPEG XL.

**Workaround:** kettle-ui decodes every image to RGBA itself, and
kettle-ui-iced hands iced the pixels (`Handle::from_rgba`), with iced's
`image-without-codecs` feature.

## Startup notification

**Needed for:** launching hob from its desktop file: the window should
get focus, and the launcher should stop waiting (GNOME Shell shows a busy
cursor, and the dock doesn't show the window, until it times out).

iced never reads the launcher's token (`XDG_ACTIVATION_TOKEN` on Wayland,
`DESKTOP_STARTUP_ID` on X11) or passes it to winit, which would use it
(iced#3317).

**Workaround:** iced is patched (`[patch.crates-io]` in Cargo.toml) with
our fork's `startup-notify` branch: the 0.14 branch plus one commit that
gives the first window the token from the environment. To be sent
upstream as a fix for iced#3317.

## Rows and tables that share their width

**Needed for:** kettle-ui's `row`, `table` and `table_horizontal`, when
the island is narrow.

iced's `row` and `table` give each child, in order, all the width left by
the ones before it, so a long label leaves nothing for what follows it,
and a child that fills the width gets none. In a table whose columns all
shrink, iced makes the first column fill and lays it out last, so its
cells overlap the next column's. Its `Row::wrap` gives every filling
child a line of its own. And the table draws lines only between cells,
not around it.

**Workaround:** our own layout widget for rows and tables (`grid.rs` in
kettle-ui-iced), which starts new lines in a row and shares the width
between table columns; the outer table lines come from a bordered
container (all borders) or rules above and below (horizontal only).
