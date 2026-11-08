# TODO

Plans and open questions. Decisions already made are in DECISIONS.md;
known bugs are in BUGS.md.

What's most important is to get the core primitives right. UI polish can
come later.

## kettle-ui core

### Dependency tracking

- Option to highlight outdated values (being recomputed, or waiting for
  derived state they depend on). `Model::is_outdated` already tells the
  backend which they are.
- What to do when a computation panics. For now, the old value stays until
  one of the inputs read before the panic changes. Alternatives: show an
  error in place of the value, or treat it as a bug and abort (at least in
  strict mode, where reading undeclared state panics on purpose but is
  currently caught like any other panic).

### Frozen islands

- Should buttons of a frozen island be shown at all?

## Widgets

The widget API is defined in kettle-ui core; kettle-ui-iced renders it.

- **`suggestions()`:** make it a full-featured autocomplete widget, with
  suggestions selectable by keyboard like in an IDE. (Currently it only
  supports clicking.)
- **Keyboard:** buttons can't get focus (iced can only focus text inputs;
  checkboxes have our own wrapper), so button keys work only while the
  island as a whole has focus. Should Up/Down also move between text
  fields, as they do from checkboxes?
- Revisit button keys. They're allowed in islands with checkboxes (but not
  text fields), yet work only once focus has moved past the last checkbox,
  so they're hard to discover there; and in a checkbox, `y`/`n` mean
  something else than a button's `[y]`/`[n]`.
- Chord keys (e.g. Ctrl+N) for buttons, which could work in islands with
  text fields.

### Images

- Opt-in lossy compression for lossless images: lossy JPEG XL can still be
  visually transparent. The key would then have to include the setting.
- Images are decoded on the UI thread when first shown, which can stall it
  for big images; decode in the background, or when the image is made.
- Color management: ICC profiles are kept when compressing, but ignored when
  showing images.
- The session is saved in full after every change, images included, so a
  session with big images is slow to save.
- hob-headless resends a live island's images with every change of the
  island; clients could get each image once.
- Animated images show only their first frame.
- hob-mcp sends JPEGs (and recompressed ones, reconstructed) as they are,
  with their Exif orientation tag. A client that ignores the tag shows a
  rotated photo sideways, and the size in the outline (after orientation)
  doesn't match. Unchecked whether MCP clients or the Claude API apply it;
  if not, decode and rotate JPEGs whose orientation isn't the default.

## kettle-ui-iced

- **Smart layout:** automatically insert all necessary gaps, so the UI looks
  good without manual spacing adjustments. The spacing itself stays fixed;
  only where the gaps go becomes automatic. (Done so far: widgets with
  nothing to show get no gaps.)
- Post-MVP: some global styling customization. It will be set by the host
  application, not the island, and may be iced-specific.

## kettle-ui-headless

- Show outdated values in the outline (relevant only if computations become
  asynchronous, e.g. for agents with timeouts).

## hob-headless and hob-cli

- Whether the text outline is the right thing to show an agent; hob-mcp
  hasn't been tried with a real agent yet.
    - The outline works fine, but simplifying it and converting to Markdown
      would reduce noise for the agent.
    - Hypothesis: for smaller models, it might also help to reduce jargon,
      e.g. "island" -> "form" or "view", "frozen" -> "non-interactive".
- hob-mcp has no tool for picking a suggestion (filling in the text has the
  same effect), and doesn't use MCP notifications or resources.
- hob-mcp could return a PNG of an island (`kettle_ui_iced::render_png`).
- Records of what an agent did, for inspecting later (hob-headless saves
  nothing; see DECISIONS.md):
  - hob-mcp could save a session file (`hob_core::Session`), built from
    the notifications it gets, to open with `hob --preview`. It
    would show only final states.
  - hob-mcp could write a log of the messages to and from hob (one JSON
    line each, with times). It would include the intermediate states its
    results leave out (e.g. each change of a progress bar).
- hob-mcp reports a changed island of the command a result is about in
  full, which is a lot of text for small changes (e.g. a progress bar).
- No completion or history over the protocol yet, and hob-cli has no line
  editing.
- hob-cli runs piped lines without waiting, so a script can't act on an
  island (`/type` before it's shown fails with "no live island"). It could
  wait for a live island before a `/` line, or have a `/wait`.
- hob-cli's `/type` takes one line, so it can't put several lines into a
  text area.
- hob-cli prints changed lines of an island only if no lines were added or
  removed; otherwise it prints the whole island again.

## hob-core (command interpreter)

- Longer help for a command (a paragraph, examples), beyond the usage and
  options that `help COMMAND` shows from its declared arguments.

- `!COMMAND`: there's no way to stop a shell command, or to give it input,
  and it's left running when hob exits. A pty would give programs colours
  and progress output, but would need a terminal emulator to show it.

- The working directory isn't saved with the session, and isn't shown
  anywhere but by `pwd`. The prompt, or each block's command line, could
  show it.

## Filesystem commands (hob-fs)

- More commands: `mkdir`, `touch`, `rm`, `cp`, `mv` (`rm` could ask to
  confirm, like `exit`), maybe `cd -`.
- `ls -l` (sizes, times, permissions), possibly as an island with a table
  rather than text.
- `cat` could show big files in parts, or a binary file as hex.

## Git commands (hob-git)

- Commands that change the repository, as islands: `git commit` with a
  checkbox per changed file (staging) and a text area for the message;
  `git switch` picking a branch with suggestions. Note that gix runs no
  hooks and doesn't sign commits; decide whether that's acceptable or
  whether those should run the `git` program.
- `log` with more than one line per commit (author, date, body), and
  decorations (branch names); `show`.
- `diff` of particular paths or commits; colours (needs styled text,
  which kettle-ui doesn't have).
- `branch` doesn't show upstreams or ahead/behind counts.
- Long operations (`status` in a huge repository) can't be interrupted.

## Note commands (hob-notes)

Only the first parts are done; the rest is still to be designed.

- More TODO sources (e.g. Git: uncommitted changes), and a better choice
  of which 3 items to offer than oldest first, in turn.
- A way to list, show, edit and delete notes; `title` and `done` work only
  on the last or the active note.
- Whether "starts with todo" should need a word boundary ("todos" counts
  now), and should ignore non-ASCII case.
- Picking an item works on what was read when the island was shown; a
  note changed meanwhile (e.g. done by another command) isn't rechecked.

## Logging

- `log` records from threads a command starts go outside its block; only
  those from the command's own thread go into it.
- Record times are only shown by `messages`, in local time (UTC if the
  system time zone can't be found).
- `messages` could filter by source, or colour its output.

## hob-gui (shell)

- An icon, for the desktop file (`Icon=`) and the window.

- More prompt shortcuts: Ctrl+W, Ctrl+U and Ctrl+K (delete the word before
  the cursor, or to the start or end of the line). They need the cursor
  position, which iced doesn't expose (see ICED_WISHLIST.md); the plan is
  to contribute that upstream, then pin iced to a git version.
- Ctrl+R history search. Needs a UI design: a bash-style search in the
  prompt line, or a list of matches?
- Completion works only at the end of the line (iced doesn't tell the app
  where the cursor is; see ICED_WISHLIST.md).
- Completing a path reads its directory on the UI thread, which could
  stall on a slow or network filesystem; so could a command's custom
  completer (e.g. `git log` reading refs).
- There is no way to move focus to the command prompt using only the keyboard.
- Selecting output works only within one printed text or log record; a
  selection can't span several, nor include islands.
- Scroll to the bottom when a new command is started.
