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
- **Keyboard:** buttons and checkboxes can't get focus (iced can only focus
  text inputs; see ICED_WISHLIST.md), so an island with text fields can be
  submitted by keyboard only through Enter, and a checkbox only clicked.
  Making them focusable needs our own widgets in kettle-ui-iced. Until
  then, islands without text fields have focus as a whole, with button
  keys.
- Chord keys (e.g. Ctrl+N) for buttons, which could work in islands with
  text fields.
- Enter in an island without text fields does nothing. Should it press
  the only submit button, if there's one?

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
- hob-mcp has no tool for picking a suggestion (filling in the text has the
  same effect), and doesn't use MCP notifications or resources.
- hob-mcp could return a PNG of an island (`kettle_ui_iced::render_png`).
- hob-mcp reports a live island in full whenever it changed, which is a lot
  of text for small changes (e.g. a progress bar).
- No completion or history over the protocol yet, and hob-cli has no line
  editing.
- hob-cli prints changed lines of an island only if no lines were added or
  removed; otherwise it prints the whole island again.

## hob-core (command interpreter)

- Quoting in command lines, so an argument can contain spaces.
- Full help for a command (usage, arguments, examples): where it's written
  (a longer text at registration? a declared argument list, which could
  also drive argument completion?) and what `help COMMAND` shows. For now it
  shows the one-line description.

## Logging

- `log` records from a command's thread could go into its block.
- Record times are only shown by `messages`, in local time (UTC if the
  system time zone can't be found).
- `messages` could filter by source, or colour its output.

## hob-gui (shell)

- More prompt shortcuts: Ctrl+W, Ctrl+U and Ctrl+K (delete the word before
  the cursor, or to the start or end of the line). They need the cursor
  position, which iced doesn't expose (see ICED_WISHLIST.md); the plan is
  to contribute that upstream, then pin iced to a git version.
- Ctrl+R history search. Needs a UI design: a bash-style search in the
  prompt line, or a list of matches?
- Completion works only at the end of the line (iced doesn't tell the app
  where the cursor is; see ICED_WISHLIST.md), and doesn't complete
  arguments: commands would need a way to say what they accept.
