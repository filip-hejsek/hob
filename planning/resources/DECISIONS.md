# Resources: decisions

A subproject: plans and open questions are in TODO.md here. Decisions for
the rest of hob are in ../DECISIONS.md. Nothing here is implemented yet,
and it's a draft: these are the parts agreed so far, and may still change.

## Idea

hob's take on Unix's "everything is a file": everything is a *resource*,
and a file is one type of resource. Unix reduces everything to a byte
stream; a resource keeps its type, and the type says what can be done
with it (show, list children, edit, delete, read as text, ...). Rejected:
making things literally files (Plan 9 or FUSE style, e.g. notes as files
in a mounted directory), which loses the types and islands that set hob
apart from a terminal.

Command lines are to become a scripting language. Interactive command
lines use a superset of the script syntax, with sugar that's desugared
into script syntax. Rejected: shell-like word splitting with no formal
syntax, which ends up underspecified and full of special cases.

## Hierarchy and types

- Modules register *roots* (`:notes`, ...), not type prefixes: a root's
  descendants may be of different types. Rejected: `type:rest` addresses
  (`note:3`), where everything under a prefix has one type, which rules
  out mixed hierarchies.
- Types still exist, and decide what a resource can do.
- Resources have children (`/name`) and attributes (`/:name`), e.g. a
  directory's owner.
- Modules can extend types they don't own (e.g. hob-git giving
  directories a `:git` attribute), like Rust traits: an extension must be
  imported to be used. A prelude imports the common ones, and the config
  file can set a custom prelude.
- `..` means nothing special to resources in general: it's an ordinary
  path component. Only the filesystem gives it a meaning.
- `cd` and the working directory stay filesystem only: external programs
  run in the working directory, so it must be a real one.

## Values, places and paths

- Values have three types so far: *string*, *resource* and *path*.
- A path value is relative to a resource (its base) and needn't exist;
  it's how resources that don't exist yet are passed (e.g. a file to
  create).
- A command's spec says whether each argument takes a resource, a path
  or a string.
- Expressions come in three categories: *values*, *places* and *paths*.
  Places (lvalues) are variables (`$n`) and attributes (`./dir/:owner`).
  Paths are path expressions (`./foo`, `$n/3`).
- A place coerces to a value by reading it; a path by resolving it,
  giving a resource (an error if nothing is there). There's explicit
  syntax for taking a path as a path value instead of resolving it (like
  `&` takes a place's address in Rust or C).
- Assignment takes a value, so a variable can hold any value: a string,
  a resource or a path. `$x = ./foo` holds the resource (which must
  exist), and the path syntax holds the path.
- A variable holding a resource holds its identity, not a copy of its
  contents.

## Syntax

```
statement    := command args... | literal_path args... | place '=' expression
expression   := value | place | path
place        := '$' name | expression '/:' name   -- variable, attribute
path         := root ('/' component)* | expression '/' component
root         := '/' | '.' | '~' | ':' name
value        := string | '&' path                 -- '&': proposed, see TODO.md
literal_path := root ('/' component)*
command      := [a-zA-Z0-9] [a-zA-Z0-9._-]*
```

Where a value is needed, a place or path is coerced to one (see above).

- `/` is an operator, like `.` in many languages: `$n/3` is child `3` of
  the resource in `$n`. Paths aren't plain strings.
- `/` is the filesystem root, `.` the working directory, `~` the home
  directory, and `:name` a registered root.
- Variables have a `$` sigil, so a bare relative word can stay a file
  path in interactive use (`cat README.md`). Rejected: variables without
  a sigil, which would make files in the working directory (the most
  common thing to name) need `./`.
- Attributes are `/:name`: `:` is special only at the start of a
  component, so filenames with colons (`log-10:00`) still work. Rejected:
  `dir:owner` (clashes with such filenames) and `dir@owner` (hard to type,
  and visually distracting).
- `=` is an assignment, and needs no spaces around it. Commands and
  places look different, so a statement's kind is known from its syntax
  (`echo = .` is a command, `$x = .` an assignment).
- Only a literal path can be run as a statement (`./build.sh a b`).
  Anything with a variable or attribute in it needs `run`
  (`run $program a b`); a place not followed by `=` is an error that says
  so. This keeps `$x = .` unambiguous.
- Roots aren't places: `:notes = ...` doesn't parse.

## Interactive syntax

- In interactive command lines, an argument is parsed according to the
  type the command's spec expects (e.g. a bare word given where a path is
  expected is a path relative to `.`). Scripts don't have this: they use
  the canonical syntax, where every value's type is unambiguous.
