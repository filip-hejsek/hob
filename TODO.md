# TODO

What's most important is to get the core primitives right. UI polish can
come later.

## kettle-ui core

### Dependency tracking for derived state

Right now every derived value is recomputed after *any* field change, and
each change starts a new computation even if one is still running (results
of outdated ones are discarded). The plan:

- **Track dependencies automatically.** Record every field a computation
  reads, and recompute only when one of them changes. No boilerplate, and
  precise even with conditional reads: if a checkbox decides whether a field
  is read, that field is only a dependency while it matters.
- **At most one computation in flight per derived value.** Computations run
  asynchronously on a background thread, so fields can change while one is
  running. Don't start another one then. Instead, each computation records
  the values it read (a version ID or the value itself); when it finishes,
  compare them with the current values and rerun if any differ.
  - Open question: show the outdated result while the rerun is in progress?
    For search-as-you-type, that would make results appear sooner.
- **Dependencies on other derived values are declared upfront.** They also
  determine the order in which derived values are computed.
- A computation sees a *snapshot*, so the field values it reads are
  consistent.

Rejected alternative: declaring field dependencies upfront, e.g.
`derive("matches", &["fruit"], |f| ...)`. It's explicit, but it's
boilerplate, the list can drift from what the function reads, and
conditional reads must be over-declared.

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

## hob-gui (shell)

- Bug: the empty tab row and empty status line leave gaps.
