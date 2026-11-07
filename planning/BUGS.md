# Bugs

Known defects in the current code. Plans and open questions are in
TODO.md.

## kettle-ui core

- Frozen islands restore infinities as NaN. They could be saved as strings
  or tagged numbers instead.

## kettle-ui-iced

- Text fields of a frozen island are low-contrast (iced's disabled style),
  which makes the result hard to read.

## hob-headless and hob-cli

- Output in hob-cli interferes with the prompt and with what the user is
  typing. Stopgap: the prompt is shown only after output has paused for
  50 ms (and not at all if stdin isn't a terminal), and output overwrites
  it. A real fix needs line editing that redraws the input line.
- Log records are kept in memory without limit (for `messages`), like in
  the GUI.
- Derived state is computed on the server thread, so a slow computation
  holds up everything else (see kettle-ui-headless in DECISIONS.md).

## hob-mcp

- The `reports_crashes` test is flaky (failed once in about ten runs): the
  result after `crash` lacked "[2] was interrupted" (the `crash` command
  itself). Not investigated.

## Logging

- A dependency that logs many different messages (e.g. with a changing
  number in them) still adds them all; they may need a limit.

## hob-gui (shell)

- PageUp and PageDown in a text area move its cursor and also scroll the
  scrollback (the shell's bindings work anywhere).
- Widget state in the scrollback is matched by position, so when `clear`
  or Ctrl+L removes blocks before a running one, its live island may get
  another widget's state (focus, cursor). A `keyed_column` keyed by block
  id would fix it.
- Log records cleared by `clear` are kept only in memory, so `messages`
  no longer lists them after a restart.
- The scrollback grows without limit, and so does the session file, which
  is rewritten in full on every change.
- Edits in a live island are only saved on close (or with the next
  scrollback change), so a crash loses them.
