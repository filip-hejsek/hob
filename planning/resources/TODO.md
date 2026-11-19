# Resources: TODO

Decisions are in DECISIONS.md here. Everything below is a proposal or an
open question, not agreed yet.

## Values

- Path to resource: proposal: path expressions always build a path (a
  base resource plus components, no I/O), and resolving it gives a
  resource, failing if nothing is there. Resolving happens only where a
  resource is needed: an argument whose spec asks for one, assignment to
  a variable, and the left side of `/:attr`. Strings never become paths
  (or paths strings) without being asked to.
- Can variables hold strings and paths too (by value)?
- How long variables live and who sees them: maybe like the working
  directory (shared by the interpreter, each command gets a snapshot when
  it starts). Saved with the session?
- Identity of resources whose target can move: does a variable holding a
  branch follow the branch, or the commit it was on? Proposal: identities
  never copy contents; snapshots (a commit) are resources of their own.

## Syntax

- Bare words in canonical argument position: proposal: a string limited
  to the `command` characters, for subcommands and choices (`notes list`).
  `cat foo` in a script is then a type error.
- Options (`-a`, `--name`, `--name=value`) need a token kind of their own
  in canonical syntax; then no other value can start with `-` (negative
  numbers, if numbers become values).
- String quoting in canonical syntax: one quote style or two, escapes,
  interpolation (`"$x"`).
- The filesystem root's attributes: proposal: `/:owner`, the `/` of `/:`
  doubling as the root, rather than `//:owner`.
- `use` for importing extensions in scripts.
- Should `run` also call resources in general (a "call" capability), and
  could commands be resources (`:commands/ls`), with `ls -a` as sugar?
- Whether `cd` resolves `..` lexically (as now) or follows the
  filesystem's meaning, or is a special case.

## Interactive sugar

- Proposal: an argument's spec names a base, and a bare word is a path
  relative to it: file arguments have base `.` (`cat foo` is
  `cat ./foo`), note IDs base `:notes` (`notes show 3` is
  `notes show :notes/3`, replacing `#3`). Completion lists the base's
  children.
- Free text is joined into one string (`note call Ada` is
  `note "call Ada"`), and `!git status` is `sh "git status"`.
- A way to see a line's canonical form (e.g. to copy it into a script);
  history keeps what was typed.

## Capabilities

- Which ones, and the generic commands that use them: `show`, `ls`,
  `edit`, `rm`, `info`. Do the specific commands (`notes show`,
  `git log`) stay, or become thin wrappers?
- Name clashes between extensions from different modules (two adding
  `:owner`).

## Later

- Links in output: commands print references to resources, which the GUI
  makes clickable and an agent can pass back, e.g. `^1` for a link in
  earlier output (which block?). Needs a link widget in kettle-ui.
- Passing lists of resources between commands, like nushell or
  PowerShell.

## First steps

- A registry of roots and types in hob-core, and `Resource` and `Path`
  argument kinds, with `file` and `note` as the first types.
