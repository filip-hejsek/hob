# TODO

What's most important is to get the core primitives right. UI polish can
come later.

## kettle-ui core

### Dependency tracking for derived state

Done. Decisions:

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
  inputs change.

Rejected alternative: declaring field dependencies upfront, e.g.
`derive("matches", &["fruit"], |f| ...)`. It's explicit, but it's
boilerplate, the list can drift from what the function reads, and
conditional reads must be over-declared.

Open:

- Option to highlight outdated values (being recomputed, or waiting for
  derived state they depend on). `Model::is_outdated` already tells the
  backend which they are.
- What to do when a computation panics. For now, the old value stays until
  one of the inputs read before the panic changes. Alternatives: show an
  error in place of the value, or treat it as a bug and abort (at least in
  strict mode, where reading undeclared state panics on purpose but is
  currently caught like any other panic).

### Frozen islands

Done: a `FrozenIsland` has fixed fields and state. It's created with
`IslandBuilder::freeze` (derived state is computed once, on the calling
thread) or as a snapshot of a live island (`Model::freeze`). Meant as a
building block, e.g. for the scrollback. Decisions:

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

Open:

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

### Layout

- Spacing is fixed at 10 px.
- **Smart layout:** automatically insert all necessary gaps, so the UI looks
  good without manual spacing adjustments. The spacing itself stays fixed;
  only where the gaps go becomes automatic.
- Bug: an empty `list` still takes up a spacing gap in its parent column.
  Will probably be fixed by the smart layout.

### Styling

- The default theme for now. Styling options are deliberately minimal.
- Post-MVP: some global styling customization. It will be set by the host
  application, not the island, and may be iced-specific.

## kettle-ui-headless

Done: acts like a user (type, toggle, pick a suggestion, press a button by
label) and renders the island as an indented text outline. Decisions:

- Derived state is computed synchronously on the calling thread, so tests are
  deterministic: after an action returns, everything derived from the fields
  is up to date. The cost: a slow or hanging computation blocks the caller.
- `StateHandle` updates are applied only by `poll` or `wait_until`, never
  implicitly.
- Buttons are addressed by label; several buttons with the same label are
  fine if they do the same thing.

Open:

- Agent interface: how an agent talks to it (MCP server? JSON over stdio?),
  and whether the text outline is the right thing to show it.
- Show outdated values in the outline (relevant only if computations become
  asynchronous, e.g. for agents with timeouts).

## hob-core (command interpreter)

- Commands run on their own thread and block in `Context::show`. They may
  become async later; blocking keeps it simple for now.
- Command lines are split at whitespace; there's no quoting yet, so an
  argument can't contain spaces.
- A command error or panic is printed as `<command>: <error>`.

## Logging

Done. Decisions:

- Commands log with `ctx.error/warn/info/debug`; the records go into the
  command's block. A command's returned error or panic (with its message)
  is logged as an error.
- Everything else uses the `log` crate (nearly all dependencies do; winit's
  `tracing` events reach it too). hob-gui's backend turns records into
  notices in the scrollback, outside any block. Errors and warnings also go
  to stderr. Not `tracing`: spans aren't needed, since commands log through
  their context.
- Our crates log from `info` up (`HOB_LOG` changes the level), dependencies
  from warnings up. Only error and warning notices are shown; info notices
  are kept for later.
- Ephemeral records are shown but not saved: debug records, records from
  dependencies, and errors saving the session.
- A failed save is reported once until the error changes, and the report
  doesn't cause another save. When saving works again, that's noted.
- Records reach the shell through a channel, so logging during a save
  doesn't touch the session being saved.

Open:

- A `log` command to show all records, including hidden info notices.
- `log` records from a command's thread could go into its block.
- Dependency warnings can be noisy (winit warns on every start under Xvfb);
  maybe deduplicate them, or show them more quietly.
- kettle-ui's warnings are lost when no logger is installed (e.g. in tests).
- Record times are saved but not shown.

## hob-gui (shell)

Works like a terminal: a scrollback with the prompt below it. Decisions:

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
  and a file that can't be read is moved aside to `*.bad`. A single island
  that can't be read is shown as an error and saved back unchanged, so the
  rest of the session still loads.
- Commands still running when the session was last saved are marked as
  interrupted after a restore.
- The prompt has history (Up/Down; the line being typed is kept as a draft)
  and Tab completion of command names: unambiguous part completed, the
  candidates listed below the prompt if there are several. Both live in
  hob-core.

Open:

- Focus: nothing moves focus to a newly shown island, or back to the prompt
  afterwards.
- The scrollback grows without limit, and so does the session file, which
  is rewritten in full on every change.
- Edits in a live island are only saved on close (or with the next
  scrollback change), so a crash loses them.
- Completion works only at the end of the line (iced doesn't tell the app
  where the cursor is; see ICED_WISHLIST.md), and doesn't complete
  arguments: commands would need a way to say what they accept.
