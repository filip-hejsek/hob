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

## hob-gui (shell)

- Command output goes to the status line, which shows only the latest
  message. A scrollback is planned, but needs building blocks that don't
  exist yet.
- Bug: the empty tab row and empty status line leave gaps.
