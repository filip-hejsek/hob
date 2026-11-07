# Decisions

Design decisions made so far, by area, with rejected alternatives where
they're worth remembering. Open questions and plans are in TODO.md.
Known bugs are in BUGS.md.

## kettle-ui core

- The widget tree is fixed once built. kettle-ui is deliberately small:
  options are added only when needed.
- `text_area` is a multi-line text field. Its value is text, like a text
  field's (`Fields::text`, `set_text`), so it counts as a text field
  everywhere: suggestions can target it, and an island with one can't have
  button keys. Headless clients see `text_area` in the outline, with the
  value on one line (Debug-escaped).
- Parts of the tree are hidden with `when(state, widget)`, shown only while
  the named state is `Value::Bool(true)` (missing or another type hides
  it). Showing and hiding doesn't change the tree's shape, and a hidden
  field keeps its value. Rejected: hiding labels by setting them to `""`
  (still works, but implicit and only for labels).
- Buttons can have a key, a lowercase ASCII letter, shown as ` [y]` after
  the label, like in terminal prompts. A fixed label gives it at its end
  (`submit_button("Yes [y]")`, via `From<&'static str> for ButtonLabel`).
  Labels made at runtime use `ButtonLabel::new(text).key('y')`, which
  parses nothing; `String` isn't accepted, to discourage parsing runtime
  data (it can't be forbidden). A fixed label ending in a bracketed letter
  also needs the builder. `build` rejects keys that aren't lowercase
  letters, duplicate keys, and keys in an island with text fields, which
  take typed letters (chord keys like Ctrl+N could lift that later). The
  key is a separate field of the tree (`key`, left out when absent), so
  headless clients get it as data; the outline shows `button "Yes" [y]`.
  Rejected: an explicit key parameter on every button function.
- Besides submit and cancel buttons there's a plain `button(id, label)`:
  the island reports `Outcome::Pressed(id, fields)`. Enter never presses
  it (only a submit button), so a confirmation of a dangerous action must
  use it rather than a submit button, which Enter would press by accident
  (the `confirm` demo does). Buttons with the same id are allowed; they do
  the same thing. In frozen-island JSON its action is `{"press": id}`.
- `button_danger(id, label)` is a `button` shown in red (iced's
  `button::danger` style). It's a `danger` flag on the button in the tree
  (left out of JSON when false), and the outline adds `(danger)`. Only
  plain buttons get it: a dangerous action shouldn't be a submit button.

### Dependency tracking for derived state

- Field dependencies are tracked automatically: a computation recomputes
  only when a field it read last time changes. Read fields are compared by
  value, named state by version.
- At most one computation in flight per derived value; when it finishes, it
  reruns if anything it read has changed.
- The outdated result is shown while the rerun is in progress (it's still
  newer than what it replaces), so search-as-you-type results appear sooner.
- Dependencies on named state are declared upfront
  (`derive_with_state`) and may be derived or set state. A derived value
  waits until the derived state it depends on is up to date.
- A computation sees a snapshot of its inputs.
- A panicking computation keeps the old value and is retried when its
  inputs change (for now; see TODO.md).

Rejected alternative: declaring field dependencies upfront, e.g.
`derive("matches", &["fruit"], |f| ...)`. It's explicit, but it's
boilerplate, the list can drift from what the function reads, and
conditional reads must be over-declared.

### Frozen islands

A `FrozenIsland` has fixed fields and state. It's created with
`IslandBuilder::freeze` (derived state is computed once, on the calling
thread) or as a snapshot of a live island (`Model::freeze`).

- Named "frozen" rather than "passive": it covers both ways of making one.
- Backends render it with the same code as live islands, through the
  `Contents` trait; kettle-ui-iced disables all interaction.
- A snapshot keeps the outdated marks of derived values.
- `IslandBuilder::set_text` and `set_checked` set initial field values, for
  live islands too.
- Serializable with serde, for saved sessions. Values and field values are
  plain JSON (strings, numbers, lists, bools); maps are ordered, so output is
  stable. Deserializing checks the field values against the tree. JSON has
  no NaN or infinity: they're saved as `null`, which is restored as NaN.
- `Value` includes bools (`Value::Bool`), saved as JSON booleans and shown
  as `true`/`false`; used by `when`.

## kettle-ui-iced

- Spacing is fixed at 10 px.
- A widget with nothing to show (empty label, list or suggestions with no
  items, a column or row of such widgets) gets no gaps around it. The check
  is `kettle_ui::widget::is_visible`, so other backends can share it; it
  also covers `when`.
- Hidden widgets stay in the iced tree, at zero size: iced matches widget
  state (focus, cursor) to children by position, so removing one could reset
  the widgets after it. Columns and rows use spacing 0, and every child is
  wrapped in a container whose padding adds the gap before it. A hidden
  `when` child is also disabled, so a focused text field stops taking input.
- The text fields of a live island have iced Ids from `Id::unique()`, made
  in `Island::new` and kept by the `Island` (in tree order), so they can't
  clash with the host's Ids or another island's, and the host needn't pick
  a namespace. The host gets them with `Island::field_id`, and
  `Island::has_focus` tells whether one of them is focused. Frozen islands
  have no Ids: they're disabled. Rejected: string Ids from a namespace the
  host passes in, or from a counter; a container Id per island with iced's
  `operation::scope` (a scoped `focus` doesn't unfocus widgets outside the
  island, such as the prompt).
- A live island handles some keys itself, in a wrapper widget that sees
  key events before the island's widgets: Enter in a text field focuses
  the next shown one (fields hidden by `when` are skipped), and in the last
  one submits, if there's a submit button; Escape cancels while the island
  has focus, if there's a cancel button (otherwise the field just loses
  focus, as in iced); a button's key presses it while the island has
  focus as a whole. Rejected: handling these in the host through a subscription; by
  then a text input has already handled Escape (it unfocuses), so the host
  can't tell which island had focus.
- An island without fields can have focus as a whole: the wrapper is
  focusable, with its own `Id::unique()`, and behaves like a text input
  (focused by a click on it or by iced's focus operations, unfocused by a
  click elsewhere), so the host's focus checks see it like any other
  focusable widget. It's shown by a thin primary-coloured border around the
  island. `Island::focus` focuses the first shown field, or the island
  as a whole. Enter submits while the island as a whole has focus (see
  `button` for answers Enter mustn't give).
- Checkboxes can have focus: iced's checkbox in our own wrapper widget,
  which is `Focusable` and has an Id like a text field (checkboxes count as
  fields for `Island::field_id`, `has_focus` and Enter). The island's
  wrapper handles their keys: Space toggles; Enter moves on or submits,
  as in a text field; `y` and `n` check and uncheck and move on, but never
  submit: after the last field, focus goes to the island as a whole, where
  Enter submits. That's the only way an island with fields gets focus as a
  whole (a click inside it doesn't), and Up from there goes back to the
  last field. Up and Down move between fields. A focused checkbox has a
  thin primary-coloured border around it and its label (like the island's
  focus border), and shows a muted `[y/n]` after it, whose space is always
  kept so the layout doesn't move. The border, focus operations and clicks
  use the checkbox's own bounds, without the hint's space. Button keys work only while the island as a whole has
  focus, so `y`/`n` in a checkbox never clash with them.
- A text area is iced's `text_editor` inside our own widget, which keeps the
  editor's `Content` in its widget state and takes the value as a string,
  like `text_input`. The `Content` is remade only when the value changes
  from outside (e.g. a suggestion), so the cursor survives rebuilds. Live,
  frozen and offscreen islands use the same widget. Rejected: `Content`s
  kept by the `Island` (frozen and offscreen islands have none, and every
  change from outside would need syncing); a plain bordered text for frozen
  islands (another look to keep in step). It's 4 lines high when empty and
  grows with its text, without its own scrolling, since the scrollback
  scrolls.
- In a text area, Enter starts a new line; Ctrl+Enter (Cmd+Enter on macOS)
  does what Enter does in a text field: focuses the next shown field, or
  submits in the last one. Ctrl+Enter works in text fields too.
- Tab doesn't move focus, deliberately unlike desktop conventions: in the
  prompt it completes, and islands move between fields with Enter.
- The default theme. Styling options are deliberately minimal; global
  customization, if any, will be set by the host application, not the
  island.
- Offscreen rendering (`render_png`, and the `kettle-render` tool for
  island JSON) draws the island directly with iced's tiny-skia `Headless`
  renderer and the bundled Fira Sans (iced's `fira-sans` feature, which only
  adds the font; hob-gui's default font is unchanged), so the output is the
  same on every machine. The image is as tall as the island. It uses the
  live look by default, since that's what the island looked like to the
  user; `--frozen` gives the disabled look of the scrollback. Not
  `iced_test::Simulator`: it's made for tests (fixed 1024×768 size and 2×
  scale, snapshots compared with PNG files), not for producing images.

## kettle-ui-headless

Acts like a user (type, toggle, pick a suggestion, press a button by label)
and renders the island as an indented text outline.

- Derived state is computed synchronously on the calling thread, so tests are
  deterministic: after an action returns, everything derived from the fields
  is up to date. The cost: a slow or hanging computation blocks the caller.
- `StateHandle` updates are applied only by `poll` or `wait_until`, never
  implicitly.
- Buttons are addressed by label; several buttons with the same label are
  fine if they do the same thing.
- Widgets hidden by `when` are left out of the outline and can't be acted
  on, like for a user. A shown `when` is transparent in the outline. Empty
  labels and lists are still listed.

## hob-core (command interpreter)

- Commands run on their own thread and block in `Context::show`. They may
  become async later; blocking keeps it simple for now.
- Command lines are split at whitespace; there's no quoting yet.
- Commands that work with any frontend (such as `help` and `messages`) are
  built into hob-core (`hob_core::builtins`), and registered by `hob`.
- Every command is registered with a one-line description (required, so
  `help` never has gaps), written as an imperative without a full stop:
  "Print the arguments". Bare `help` lists all commands with their
  descriptions; `help COMMAND` shows one. Commands see the registered
  commands through `ctx.commands()`.
- The prompt's placeholder is a parameter of `hob_gui::run`; `hob` passes a
  hint to use `help`, since only it knows that `help` is registered.

## Logging

- Commands log with `ctx.error/warn/info/debug`; the records go into the
  command's block. A command's returned error or panic (with its message)
  is logged as an error.
- Everything else uses the `log` crate (nearly all dependencies do; winit's
  `tracing` events reach it too). A backend in hob-core (`hob_core::logger`)
  hands the records to the frontend; hob-gui turns them into notices in the
  scrollback, outside any block. Errors and warnings also go to stderr. Not `tracing`: spans aren't needed, since commands log through
  their context.
- Our crates log from `info` up (`HOB_LOG` changes the level), dependencies
  from warnings up. Only error and warning notices are shown in the
  scrollback, and of dependencies only errors: their warnings are rarely
  something the user can act on (winit warns on every start under Xvfb).
  `messages` lists the rest.
- A dependency's record is logged only once: repeats (same level, target and
  message) are dropped, also from stderr, since some dependencies warn
  on every frame.
- Ephemeral records are shown but not saved: debug records, records from
  dependencies, and errors saving the session.
- A failed save is reported once until the error changes, and the report
  doesn't cause another save. When saving works again, that's noted.
- Records reach the shell through a channel, so logging during a save
  doesn't touch the session being saved.
- kettle-ui prints its warnings to stderr when no logger is installed
  (`log::max_level()` is still `Off`), e.g. in tests, where the harness
  shows them for failing tests. It doesn't install a logger itself, since
  that's the application's choice.
- `messages [LEVEL]` lists all records (not `log`, which sounds like it
  writes one), with time, level and source, from `info` up by default. It
  gets the records from the frontend with `ctx.log_records()`. The list is
  printed as plain text, so it's a snapshot, and it's not coloured.

## hob-headless (frontend without a window)

- The client starts it as a child process (`hob --headless`) and talks to
  it over stdin and stdout; its lifetime follows the client. Rejected for
  now: a Unix socket server that clients attach to, which needs socket
  paths, cleanup, and rules for what several clients see.
- The protocol is JSON-RPC 2.0, one message per line: a standard format
  that clients in other languages can use with existing libraries, and close
  to MCP. The client calls methods (run a command line, act on an island);
  the frontend sends notifications about what commands do. Notifications
  caused by a call come before its reply.
- Islands are sent structured: as frozen islands (the format of saved
  sessions), again after every change, and frozen once submitted or
  cancelled. Clients render them; hob-cli prints the text outline.
- `hob` is the binary, with a `--headless` flag, so both frontends get the
  same commands. hob-headless itself doesn't know about specific commands.
- Updates of live islands from `StateHandle`s wake the server through a
  waker (`Island::poll_waking`); no polling on a timer.
- Nothing is saved; there's no session.

## hob-cli (demo client)

- Lines are run as hob commands; lines starting with `/` act on islands
  (`/type`, `/check`, `/pick`, `/press`, `/show`), by default on the newest
  live island, or on `#N`.
- Output is prefixed with the block number, like `[3] hi`. After the first
  time, an island is printed only as its changed lines.

## hob-mcp (MCP server)

- A bridge: it starts `hob --headless` and talks to it as a client (with
  `hob_headless::client`, shared with hob-cli), so agents get the same
  commands as the GUI.
- MCP is implemented by hand (initialize, ping, tools/list, tools/call):
  that's all tools need, and the official Rust SDK would bring in tokio.
- Tools: `run` (a command line), `fill` (any number of text fields and
  checkboxes at once), `press` (a button), `state` (overview), `wait` (for
  something to happen, or a command to finish). `run` and `press` wait until
  the command finishes or shows an island, so a step usually takes one call.
- Updates come with tool results, not as MCP notifications (which clients
  mostly ignore): everything that happens is collected, and every result
  starts with what happened since the last one. Repeated events are counted
  ("changed (3 times)"), and things that happen together are reported
  together (up to 0.5 s of settling).
- Islands are shown as kettle-ui-headless's text outline, with the island
  number to act on; a live island after every change, a printed one once.
- Errors are tool results with `isError`, so the agent sees them.

## hob-gui (shell)

Works like a terminal: a scrollback with the prompt below it.

- Each command line gets a block in the scrollback, with what the command
  printed and the islands it showed, in order. Concurrent commands each add
  to their own block.
- Live islands are shown inline and freeze in place once submitted or
  cancelled. There are no tabs.
- The scrollback sticks to the bottom while it's scrolled there.
- A line with an unknown command is still added, with the error.
- What commands print also goes to stdout.
- The session (history and scrollback) is saved as versioned JSON after every
  change to the scrollback and on window close, and restored on start. Live
  islands are saved frozen, as they are at that moment. The file is in the
  platform state directory (`HOB_SESSION` overrides it); writes are atomic,
  and a file with bad contents is moved aside to `*.bad`. A single island
  that can't be read is shown as an error and saved back unchanged, so the
  rest of the session still loads.
- Commands still running when the session was last saved are marked as
  interrupted after a restore.
- `clear` (a builtin; commands call `ctx.clear()`) clears the scrollback,
  including its own block, but keeps the blocks of other commands still
  running, so their live islands aren't cancelled. Log records of cleared
  items are kept in memory for `messages`. In hob-headless, it's a
  `cleared` notification, since the client owns the scrollback: hob-cli
  clears the terminal, hob-mcp reports it.
- `exit` (a builtin; commands call `ctx.exit()`) works like closing the
  window: the session is saved and other running commands are marked
  interrupted, but not `exit` itself. In hob-headless, it's an
  `exit_requested` notification, since the client decides when the frontend
  ends (by ending its input): hob-cli quits, hob-mcp only reports it, since
  an agent can't end the MCP server. The frontend treats a broken pipe on
  its output like the end of its input, so a client can just quit.
- If other commands are running, `exit` first asks to confirm, listing
  them; "Yes" is a plain red button, so Enter can't confirm. It learns of
  them through `Context::running_commands`, a request both frontends
  answer. Without other commands, it exits at once, without asking.
  `jobs` prints the same list, one command line per line.
- Key bindings are a table (the `keys` module) from a key with exact
  modifiers to a named action, kept apart from what the actions do, so it
  can become configurable. Letters are compared in lowercase. History and
  completion work only while the prompt has focus; the others anywhere:
  PageUp/PageDown scroll the scrollback by a bit less than a page, and
  Ctrl+L clears it like `clear`, keeping the blocks of running commands.
  Ctrl+D exits like `exit`, but only in the focused, empty prompt, as in
  a terminal; otherwise it does nothing. It never asks: while commands are
  running, it doesn't exit, but adds an ephemeral warning ("Not exiting:
  LINE [and N other commands] are running"), naming the newest command
  waiting for input, else the newest running one, and moves focus to that
  command's island, scrolling it into view as little as needed (its top
  first, if it doesn't fit). The island's bounds come from an operation
  (it's a focusable with the island's Id), then a second one scrolls.
  Closing the window still exits without asking.
  Escape has no binding.
- A newly shown island gets focus, unless something other than the empty
  prompt has it, so an island that appears later doesn't take keys meant
  for something else. When an island freezes and nothing has focus, the
  prompt gets it back. Both check with an operation that always answers
  (iced's `find_focused` gives no answer when nothing is focused).
- The prompt has history (Up/Down; the line being typed is kept as a draft)
  and Tab completion of command names: unambiguous part completed, the
  candidates listed below the prompt if there are several. Both live in
  hob-core.
- Trailing whitespace is trimmed from a submitted line, so the space that
  completion adds doesn't reach the command or the history. It's invisible,
  and words are split at whitespace anyway, so it can't mean anything.
- A line starting with whitespace is private, like bash's `ignorespace`:
  neither it nor its output is saved, and its block says so. Unlike bash,
  Up/Down still offer it while its block is in the scrollback; `clear`
  forgets it. The leading space stays in the history line, so a recalled
  private line stays private when run again. Privacy is read from the line,
  so `History` and the blocks need no flag.
- The prompt has no box: a bold `>` (the text input's icon) like the command
  lines in the scrollback, with the typed text aligned with theirs. Focus is
  shown by the `>`'s colour (primary when focused, dim otherwise); as the
  icon, its colour comes from the input's style, so the shell doesn't need
  to track focus.
- End-to-end tests run the real shell (`application()`, the same builder
  `run` uses) in `iced_test`'s `Emulator`, without a window, with test-only
  commands. The emulator runs in `Mode::Immediate`, and a small harness
  retries each expectation until it holds or 5 s pass. Rejected: `Mode::Zen`
  and `.ice` test files, which wait for all tasks to finish; a command
  waiting for an island never finishes by itself. Before typing after
  focus should have moved, a test waits until no actions came for 50 ms.
  Keys handled through subscriptions (the shell's bindings) can't be
  tested this way (see ICED_WISHLIST.md); they're tested on `History`,
  `Interpreter::complete` and the bindings table, Ctrl+L by calling
  `update` directly, and all checked by hand in a real window. Keys that
  islands handle themselves go through the emulator.
