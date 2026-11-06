# TODO

Plans and open questions. Decisions already made are in DECISIONS.md.

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

- Infinities are restored as NaN too, which isn't ideal. They could be saved
  as strings or tagged numbers instead.
- Styling: iced's disabled style makes text fields low-contrast, which isn't
  ideal for reading a result. Also, should buttons of a frozen island be
  shown at all?

## Widgets

The widget API is defined in kettle-ui core; kettle-ui-iced renders it.

- **`suggestions()`:** make it a full-featured autocomplete widget, with
  suggestions selectable by keyboard like in an IDE. (Currently it only
  supports clicking.)

## kettle-ui-iced

- **Smart layout:** automatically insert all necessary gaps, so the UI looks
  good without manual spacing adjustments. The spacing itself stays fixed;
  only where the gaps go becomes automatic.
- Bug: an empty `list` still takes up a spacing gap in its parent column.
  Will probably be fixed by the smart layout.
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
- hob-mcp reports a live island in full whenever it changed, which is a lot
  of text for small changes (e.g. a progress bar).
- No completion or history over the protocol yet, and hob-cli has no line
  editing.
- Output in hob-cli interferes with the prompt and with what the user is
  typing. Stopgap: the prompt is shown only after output has paused for
  50 ms (and not at all if stdin isn't a terminal), and output overwrites
  it. A real fix needs line editing that redraws the input line.
- Log records are kept in memory without limit (for `messages`), like in
  the GUI.
- hob-cli prints changed lines of an island only if no lines were added or
  removed; otherwise it prints the whole island again.
- Derived state is computed on the server thread, so a slow computation
  holds up everything else (see kettle-ui-headless in DECISIONS.md).

## hob-core (command interpreter)

- Quoting in command lines, so an argument can contain spaces.
- Full help for a command (usage, arguments, examples): where it's written
  (a longer text at registration? a declared argument list, which could
  also drive argument completion?) and what `help COMMAND` shows. For now it
  shows the one-line description.

## Logging

- `log` records from a command's thread could go into its block.
- A dependency that logs many different messages (e.g. with a changing
  number in them) still adds them all; they may need a limit.
- Record times are only shown by `messages`, in local time (UTC if the
  system time zone can't be found).
- `messages` could filter by source, or colour its output.

## hob-gui (shell)

- Focus: nothing moves focus to a newly shown island, or back to the prompt
  afterwards.
- The scrollback grows without limit, and so does the session file, which
  is rewritten in full on every change.
- Edits in a live island are only saved on close (or with the next
  scrollback change), so a crash loses them.
- Completion works only at the end of the line (iced doesn't tell the app
  where the cursor is; see ICED_WISHLIST.md), and doesn't complete
  arguments: commands would need a way to say what they accept.
