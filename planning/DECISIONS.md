# Decisions

Design decisions made so far, by area, with rejected alternatives where
they're worth remembering. Open questions and plans are in TODO.md.

## kettle-ui core

- The widget tree is fixed once built. kettle-ui is deliberately small:
  options are added only when needed.
- Parts of the tree are hidden with `when(state, widget)`, shown only while
  the named state is `Value::Bool(true)` (missing or another type hides
  it). Showing and hiding doesn't change the tree's shape, and a hidden
  field keeps its value. Rejected: hiding labels by setting them to `""`
  (still works, but implicit and only for labels).

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
- The prompt has history (Up/Down; the line being typed is kept as a draft)
  and Tab completion of command names: unambiguous part completed, the
  candidates listed below the prompt if there are several. Both live in
  hob-core.
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
  waiting for an island never finishes by itself. Keys handled through
  subscriptions (Up/Down/Tab) can't be tested this way (see
  ICED_WISHLIST.md); they're tested on `History` and
  `Interpreter::complete`, and checked by hand in a real window.
