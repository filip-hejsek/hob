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

### Images

- `image(state)` shows named state that's a `Value::Image`, so images can
  change like other state (set through a `StateHandle`, or derived, as in
  the `swatch` demo). Rejected: an image fixed in the tree (can't change),
  or both like `label` (more code than needed).
- An `Image` is made from 8-bit RGBA pixels, an image file's contents (PNG,
  JPEG, GIF, WebP, JPEG XL) or a file, and always has an alt text. From a
  file, it keeps the absolute path. Making one compresses it, on the
  caller's thread (a command's, or a derived computation's).
- Images are always kept compressed (`ImageData`). Lossy ones are kept as
  they came, except JPEG: JPEGs and lossless images (RGBA, PNG) are
  recompressed losslessly as JPEG XL when that's smaller. JPEG XL's JPEG
  recompression keeps what's needed to restore the original file exactly.
  PNGs keep their bit depth and ICC profile; animated PNGs and GIFs are
  kept as they are. Displaying an animated image shows its first frame.
- Each image has a key (`ImageKey`): a 128-bit BLAKE3 hash of the data the
  island gave (pixels with their size, or the file contents), not of what's
  stored, so compression can change, or be non-reproducible, without
  changing keys.
- JPEG XL comes from the system libjxl, through our own minimal bindings
  (`kettle-jxl`, which builds with pkg-config). Rejected: `jpegxl-rs` (GPL,
  which doesn't fit hob's license), pure-Rust encoders (`zune-jpegxl` has no
  JPEG recompression, and its lossless output is often bigger than PNG),
  `gamut-jxl` (needs a newer Rust than our MSRV). libjxl also decodes JPEG
  XL for display; other formats are decoded by the `image` crate.
- An image serializes as an object (the only `Value` that does): `key`,
  `alt`, then `format`, `width`, `height` and `data` (base64) if the data is
  there, and `path` if it's from a file. An image without data is shown as
  "Image not available" with its alt text; one that can't be decoded, as
  "Image can't be shown". Elsewhere (e.g. a label of the same state) an
  image shows as its alt text.
- The session file (version 3) keeps each image's data once, in an `images`
  table by key at the end; islands keep the rest. An island can mark an
  image ephemeral (`Image::ephemeral`): it's left out of the table, and its
  path isn't saved either, so after a restart only its alt text is shown.
  Ephemeral affects only what's saved to disk, not what's shown or sent to
  clients. An unreadable island's images get their data inline when
  loaded, so it isn't lost.
- hob-headless sends images with their data, but without their paths
  unless asked (`Options::image_paths`, set by `HOB_IMAGE_PATHS=1` in `hob
  --headless`): the client may not share the file system, and paths may be
  private. With the path, a client on the same machine can use the file;
  the data is still there if it can't. The outline shows
  `image "alt" (WxH)`, with the path if there is one.
- kettle-ui-iced decodes an image once, when first shown, and keeps the
  iced handle with the `Image` (`Image::backend_cache`), so the renderer
  keeps its copy; the handle is dropped with the image. It's shown at its
  natural size, or scaled down to fit the width.

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
  wrapper handles their keys: Space toggles; Enter skips the checkboxes
  after it: it focuses the next field that isn't a checkbox, or submits if
  only checkboxes are left (going through a run of checkboxes one by one
  is what Down and `y`/`n` are for); `y` and `n` check and uncheck and move on, but never
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
- Command lines are split into words like a POSIX shell does, as far as
  quoting goes: `'…'` is literal, in `"…"` only `\"` and `\\` are escapes,
  and outside quotes `\` escapes any character; pieces next to each other
  join into one word. An unquoted `~` at the start of a word, alone or
  before `/`, is the home directory, so `'~'` is a file named `~`. There's
  no other expansion (`$VAR`, globs): nothing needs it yet. A line that
  doesn't split (an unclosed quote, a trailing `\`) is an error. Rejected:
  only double quotes, which would surprise shell users typing `'a b'` or
  `a\ b`.
- Commands that work with any frontend (such as `help` and `messages`) are
  built into hob-core (`hob_core::builtins`), and registered by `hob`.
- Every command is registered with a one-line description (required, so
  `help` never has gaps), written as an imperative without a full stop:
  "Print the arguments". Bare `help` lists all commands with their
  descriptions; `help COMMAND` shows one. Commands see the registered
  commands through `ctx.commands()`.
- Command names must start with a letter or digit (`char::is_alphanumeric`;
  `register` panics otherwise). Names starting with other characters
  (`!`, `@`, `#`, `_`, ...) are reserved for prefix commands and other
  features. A name can be quoted (`'ls'`); a quoted name never runs a
  prefix command.
- Prefix commands (`register_prefix`) are registered by a prefix that starts
  with a special character other than a quote or `\` (those quote a
  command name) and has no whitespace, and run for any line
  starting with it (after leading whitespace), getting the rest of the line
  unsplit. The longest matching prefix wins. They share the name table with
  other commands, so `help` lists them and `help !` describes one, but Tab
  doesn't complete them: completing `!` to `! ` would only get in the way.
- `!COMMAND` (a builtin) runs a shell command with `sh -c`, not `$SHELL`, so
  the syntax is the same whatever the user's login shell. It runs in the
  interpreter's working directory, with pipes for stdio and no pty. Nothing
  is written to stdin, which is closed at once, so a program reading input
  gets end of file instead of hanging. Stdout and stderr are both printed
  (as in a terminal), as they arrive, in chunks of whole lines, so fast
  output doesn't make one entry per line; non-UTF-8 bytes are replaced.
  Output is limited to 1 MiB, like `cat`, with a warning; the rest is read
  and dropped, so the program isn't blocked on a full pipe. A failing exit
  status is the command's error.
- The interpreter has a working directory, simulated: the process's own
  is never changed, since commands run concurrently in one process. It
  starts as the process's. Each command gets a snapshot when it starts
  (`ctx.cwd()`); `ctx.set_cwd` changes it for the command and for commands
  started later, but not for those already running, like a shell's
  background jobs. It lives in hob-core, not hob-fs, because any command
  that takes a path needs it. `ctx.resolve(path)` makes a path absolute.
  It doesn't expand `~`, which the command line already did (as a shell
  does, so a quoted `~` stays), and doesn't normalize, so `..` in a path
  is followed by the OS, as in a shell.
- The prompt's placeholder is a parameter of `hob_gui::run`; `hob` passes a
  hint to use `help`, since only it knows that `help` is registered.

## Filesystem commands (hob-fs)

`pwd`, `cd`, `ls` and `cat`, behaving like their Unix namesakes where
there's no reason to differ. Options are parsed like `getopt` (flags before
operands, `--` ends them); unknown ones are errors.

- `cd` resolves `..` lexically, like bash's default (`cd -L`), so
  `cd link/..` comes back. It checks that the target is a directory. No
  argument means the home directory.
- `ls` prints one name per line, sorted, with `/` after directories (as
  `ls -F` does; there are no colours), and hides dotfiles unless `-a`.
  With several paths, each directory gets a `PATH:` header. A path that
  can't be listed is logged as an error and the others are still listed.
- `cat` prints each file as one message, without its final line break.
  Files with a NUL byte are refused as not text, and files over 1 MiB as
  too big: the scrollback isn't made for that, and it's saved with the
  session. Invalid UTF-8 is replaced.
- I/O errors are shown as `PATH: message`, without Rust's `(os error N)`.

## Git commands (hob-git)

One `git` command with subcommands (`status`, `log`, `branch`, `diff`),
named like git's so they're familiar; `!git ...` still runs the real one.

- They use gitoxide (`gix`), not the `git` program: a library gives
  structured results to build islands from, and needs no `git` installed.
  Rejected: git2 (libgit2, a C library to build or find). gix's default
  features are off; only what's used is on (status, revisions, blob
  diffs), so no network code is built.
- The repository is found from the interpreter's working directory, like
  git. `status` shows paths relative to it; `diff` names files from the top
  of the worktree, as git does.
- `status` is git's long format without the hints, one message per section
  (staged, not staged, untracked; conflicts first), each sorted by path.
  Untracked directories are collapsed (`dir/`).
- `log` is like `git log --oneline`, newest first by commit time, and shows
  20 commits unless `-n N` says otherwise (with an info record saying there
  are more): the scrollback isn't made for a whole history.
- `diff` (worktree against index) and `diff --cached` (index against
  `HEAD`) print one message per file, in git's unified format with 3 lines
  of context, capped at 1 MiB like `cat`. Files are compared as stored:
  no filters (line endings), diff drivers, or function names in hunk
  headers. A file with a NUL byte is reported as binary.
- Tests build repositories with the `git` program, ignoring the user's
  config (`GIT_CONFIG_GLOBAL=/dev/null`), with fixed commit times.

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
- The window is the `hob` crate's `gui` feature, on by default; without it
  (`--no-default-features`), hob only runs with `--headless` and doesn't
  build iced. Rejected: a separate headless binary, which would need the
  command registration shared between two binaries.
- Updates of live islands from `StateHandle`s wake the server through a
  waker (`Island::poll_waking`); no polling on a timer.
- Nothing is saved; there's no session. The client owns the scrollback
  (see `clear` below), so a client that wants a record keeps it itself.
  Rejected: saving a session in hob-headless, which would need its own
  copy of the scrollback. A session wouldn't show intermediate states
  anyway: it's a snapshot, with each island as it is when saved.

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
  ("changed (3 times)"), also when other commands' events came between
  (an event is counted with the last one of its command, so each command's
  order is kept), and things that happen together are reported together
  (up to 0.5 s of settling).
- Islands are shown as kettle-ui-headless's text outline, with the island
  number to act on; a printed one once. A live island is shown in full when
  new, and after a change only in a result about its command (`run`,
  `fill`, `press`, `wait` for its block) or in `wait` without a block and
  `state`; other results only list it as changed (counted), so an island
  that keeps changing (e.g. a progress bar) doesn't fill unrelated results.
  `run`, `press` and `wait` for a block end with the command's status
  ("has finished", "is waiting for island #3").
- Errors are tool results with `isError`, so the agent sees them.
- Hob runs as a child process, started with a fixed command (`hob
  --headless` from the same directory, or the arguments), or with
  `--agent-spawns`, with the command (an argv array, no shell) the agent
  gives to the `start` tool. The agent can't give a command in the fixed
  mode, so adding the server doesn't let it run arbitrary programs. One
  hob at a time; `start` reports its PID.
- Restarting is opt-in (`--allow-restart`, implied by `--agent-spawns`).
  Without it there are no `start` and `close` tools, a command's request
  to exit only gets reported (the agent couldn't get hob back), and when
  hob crashes, the tool result reports it and hob-mcp then exits, with a
  failure status: the MCP client sees the server is gone, and reconnecting
  starts a new hob. Rejected: staying up and failing every tool call.
- Hob may end: it crashes or is killed (its stdout ends), or, with
  restarting allowed, a command asks it to exit (`exit_requested`) or the
  agent uses `close` (like the window's close button: running commands
  are interrupted, without asking). Either way its input is ended, it gets
  3 s to exit, then its process group is killed (a group, so `cargo run`
  wrappers die too). The result reports the exit status, running commands
  as interrupted, and, if it failed, the last 20 lines of its stderr (its
  stderr is passed on to hob-mcp's own throughout; reporting it all would
  repeat log records). With restarting allowed, `start` then starts a new
  one; block and island numbers start over.
- Calls to hob time out after 10 s, and its stdin is written on a thread,
  so a hob that hangs can't block the server; the agent can `close` it.
- Its tests run the test program itself as hob (`harness = false`, with a
  `--hob` mode), so crashes, kills and hangs are real processes.

## hob-gui (shell)

Works like a terminal: a scrollback with the prompt below it.

- Each command line gets a block in the scrollback, with what the command
  printed and the islands it showed, in order. Concurrent commands each add
  to their own block.
- Live islands are shown inline and freeze in place once submitted or
  cancelled. There are no tabs.
- The scrollback sticks to the bottom while it's scrolled there.
- The scrollback is a keyed column (our own `Keyed` widget), keyed by block
  id, and so is each block's output, keyed by entry index. So a live island
  keeps its widget state (focus, cursor) when `clear` removes blocks before
  it, or the log level hides records before it. Notices and notes share
  a key: they're plain text. Rejected: iced's `keyed_column`, which only
  handles one run of children added or removed (see ICED_WISHLIST.md),
  while `clear` can remove blocks on both sides of a running one.
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
- `hob --preview [FILE]` shows a saved session read-only: the shell gets no
  interpreter at all (it's optional in hob-gui), so no command can run.
  There's no prompt, only a note saying what file is shown; only scrolling
  works. The file is never written, nor moved aside if it's bad; a missing
  file is reported as an error.
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
  ends (by ending its input): hob-cli quits; hob-mcp ends hob if the agent
  may start it again (`--allow-restart`), otherwise it only reports it. The frontend treats a broken pipe on
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
  and words are split at whitespace anyway, so it can't mean anything,
  unless it's escaped or quoted (`a\ `); that's kept
  (`hob_core::trim_line_end`).
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
