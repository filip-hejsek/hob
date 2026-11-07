//! Tab completion of command lines: command names, then whatever a
//! command's [`Spec`](crate::args::Spec) says comes next.

use std::fs;
use std::path::Path;

use crate::args::{CompletionContext, Expect, Kind, Next};
use crate::line::{self, Partial};
use crate::{Commands, Run, is_prefix};

/// What [`Interpreter::complete`](crate::Interpreter::complete) made of a
/// line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    /// The line, completed as far as it's unambiguous.
    pub line: String,
    /// The possible completions, if there are several; otherwise empty.
    /// Paths are shown by their last component, like a shell does.
    pub candidates: Vec<String>,
}

/// A possible completion of the word being typed.
struct Candidate {
    /// The whole word.
    value: String,
    /// How it's listed.
    shown: String,
    /// Whether the word can go on (a directory): no space after it.
    open: bool,
}

impl Candidate {
    fn word(value: &str) -> Self {
        Candidate {
            value: value.to_owned(),
            shown: value.to_owned(),
            open: false,
        }
    }
}

pub(crate) fn complete(commands: &Commands, cwd: &Path, line: &str) -> Completion {
    let unchanged = Completion {
        line: line.to_owned(),
        candidates: Vec::new(),
    };
    let trimmed = line.trim_start();
    if (commands.keys()).any(|name| is_prefix(name) && trimmed.starts_with(name.as_str())) {
        return unchanged;
    }
    let home = line::home();
    let (words, partial) = line::split_partial(line, home.as_deref());
    let typed = partial.word.value.as_str();
    let candidates: Vec<Candidate> = match words.split_first() {
        None => (commands.keys())
            .filter(|name| !is_prefix(name) && name.starts_with(typed))
            .map(|name| Candidate::word(name))
            .collect(),
        Some((name, args)) => {
            let Some(Run::Parsed(spec, _)) = commands.get(&name.value).map(|c| &c.run) else {
                return unchanged;
            };
            let args: Vec<String> = args.iter().map(|w| w.value.clone()).collect();
            let Some(expect) = spec.expect(&args) else {
                return unchanged;
            };
            let ctx = CompletionContext {
                cwd,
                commands: commands.keys().map(String::as_str).collect(),
            };
            candidates(&expect, &ctx, typed)
        }
    };
    match candidates.as_slice() {
        [] => unchanged,
        [only] => Completion {
            line: rewrite(line, &partial, &only.value, !only.open, home.as_deref()),
            candidates: Vec::new(),
        },
        [first, rest @ ..] => {
            let len = (rest.iter()).fold(first.value.len(), |len, c| {
                common_prefix_len(&first.value[..len], &c.value)
            });
            let common = &first.value[..len];
            Completion {
                line: rewrite(line, &partial, common, false, home.as_deref()),
                candidates: candidates.into_iter().map(|c| c.shown).collect(),
            }
        }
    }
}

/// The candidates for the word being typed, sorted.
fn candidates(expect: &Expect, ctx: &CompletionContext, typed: &str) -> Vec<Candidate> {
    let words = match &expect.next {
        _ if typed.starts_with('-') && !expect.options.is_empty() => (expect.options.iter())
            .map(|long| format!("--{long}"))
            .collect(),
        Next::Nothing | Next::Value(Kind::Text) => return Vec::new(),
        Next::Value(Kind::Path) => return paths(ctx.cwd(), typed, false),
        Next::Value(Kind::Dir) => return paths(ctx.cwd(), typed, true),
        Next::Subcommands(names) => names.iter().map(|&n| n.to_owned()).collect(),
        Next::Value(Kind::Choice(choices)) => choices.iter().map(|&c| c.to_owned()).collect(),
        Next::Value(Kind::Custom(completer)) => completer.complete(ctx),
    };
    let mut words: Vec<String> = words.into_iter().filter(|w| w.starts_with(typed)).collect();
    words.sort();
    words.dedup();
    words.iter().map(|w| Candidate::word(w)).collect()
}

/// The paths that `typed` (relative to `cwd`) can be completed to: the
/// entries of its directory that start with its last component. Hidden ones
/// only if that starts with `.`. Directories end with `/`.
fn paths(cwd: &Path, typed: &str, only_dirs: bool) -> Vec<Candidate> {
    let (dir, base) = match typed.rfind('/') {
        Some(i) => typed.split_at(i + 1),
        None => ("", typed),
    };
    let Ok(entries) = fs::read_dir(cwd.join(dir)) else {
        return Vec::new();
    };
    let mut candidates: Vec<Candidate> = (entries.flatten())
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if !name.starts_with(base) || (name.starts_with('.') && !base.starts_with('.')) {
                return None;
            }
            // Follows symlinks, so a link to a directory counts as one.
            let is_dir = entry.path().is_dir();
            if only_dirs && !is_dir {
                return None;
            }
            let slash = if is_dir { "/" } else { "" };
            Some(Candidate {
                value: format!("{dir}{name}{slash}"),
                shown: format!("{name}{slash}"),
                open: is_dir,
            })
        })
        .collect();
    candidates.sort_by(|a, b| a.value.cmp(&b.value));
    candidates
}

/// The line with the word being typed replaced by `value`. If `done`, the
/// word is finished: its quote is closed and a space follows.
fn rewrite(line: &str, partial: &Partial, value: &str, done: bool, home: Option<&str>) -> String {
    let word = line::requote(value, partial.quote, done, home, partial.word.tilde);
    let space = if done { " " } else { "" };
    format!("{}{word}{space}", &line[..partial.word.start])
}

/// The length in bytes of the longest common prefix of `a` and `b`.
fn common_prefix_len(a: &str, b: &str) -> usize {
    a.char_indices()
        .find(|&(i, c)| !b[i..].starts_with(c))
        .map_or(a.len(), |(i, _)| i)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::Interpreter;
    use crate::args::{Kind, Spec};

    use super::*;

    /// An interpreter in a temporary directory with some files, and a few
    /// commands with specs.
    fn setup() -> (Interpreter, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        for file in [
            "notes.txt",
            "new file",
            ".hidden",
            "sub/inner.txt",
            "subway/x",
        ] {
            let path = dir.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        let mut i = Interpreter::new();
        i.set_cwd(dir.path());
        let ls = Spec::new()
            .flag("all", "All")
            .short('a')
            .option("width", "N", Kind::Text, "Width")
            .short('w')
            .option(
                "color",
                "WHEN",
                Kind::Choice(&["always", "auto", "never"]),
                "Color",
            )
            .arg("PATH", Kind::Path)
            .optional()
            .repeated();
        i.register_spec("ls", "Test", ls, |_, _| Ok(()));
        let cd = Spec::new().arg("DIR", Kind::Dir).optional();
        i.register_spec("cd", "Test", cd, |_, _| Ok(()));
        let names = Kind::custom(|ctx| ctx.commands().iter().map(|&c| c.to_owned()).collect());
        let git = Spec::new()
            .subcommand("log", "Test", Spec::new().arg("REV", names))
            .subcommand("status", "Test", Spec::new());
        i.register_spec("git", "Test", git, |_, _| Ok(()));
        i.register("echo", "Test", |_, _| Ok(()));
        (i, dir)
    }

    fn completion(line: &str, candidates: &[&str]) -> Completion {
        Completion {
            line: line.to_owned(),
            candidates: candidates.iter().map(|&c| c.to_owned()).collect(),
        }
    }

    #[test]
    fn completes_options() {
        let (i, _dir) = setup();
        assert_eq!(i.complete("ls --a"), completion("ls --all ", &[]));
        assert_eq!(
            i.complete("ls -"),
            completion("ls --", &["--all", "--color", "--width"])
        );
        // Not after an operand.
        assert_eq!(i.complete("ls notes.txt --a").candidates, [""; 0]);
    }

    #[test]
    fn completes_option_values() {
        let (i, _dir) = setup();
        assert_eq!(
            i.complete("ls --color a"),
            completion("ls --color a", &["always", "auto"])
        );
        assert_eq!(
            i.complete("ls --color n"),
            completion("ls --color never ", &[])
        );
        // A text value isn't completed, and isn't taken for a path.
        assert_eq!(i.complete("ls -aw n"), completion("ls -aw n", &[]));
        // Once given, the value isn't what's completed.
        assert_eq!(
            i.complete("ls -w5 no"),
            completion("ls -w5 notes.txt ", &[])
        );
    }

    #[test]
    fn completes_paths() {
        let (i, _dir) = setup();
        assert_eq!(i.complete("ls no"), completion("ls notes.txt ", &[]));
        assert_eq!(
            i.complete("ls n"),
            completion("ls n", &["new file", "notes.txt"])
        );
        assert_eq!(i.complete("ls ne"), completion(r"ls new\ file ", &[]));
        assert_eq!(i.complete("ls 'ne"), completion("ls 'new file' ", &[]));
        assert_eq!(
            i.complete(r#"ls "new f"#),
            completion(r#"ls "new file" "#, &[])
        );
        // Directories get a slash and no space; their entries are listed by
        // name.
        assert_eq!(
            i.complete("ls sub"),
            completion("ls sub", &["sub/", "subway/"])
        );
        assert_eq!(i.complete("ls sub/"), completion("ls sub/inner.txt ", &[]));
        assert_eq!(i.complete("ls subw"), completion("ls subway/", &[]));
        // Hidden files only when asked for.
        assert_eq!(i.complete("ls .h"), completion("ls .hidden ", &[]));
        assert_eq!(i.complete("ls ").candidates.len(), 4);
        // Paths go on after the first.
        assert_eq!(i.complete("ls a no"), completion("ls a notes.txt ", &[]));
    }

    #[test]
    fn completes_directories() {
        let (i, dir) = setup();
        assert_eq!(
            i.complete("cd "),
            completion("cd sub", &["sub/", "subway/"])
        );
        assert_eq!(i.complete("cd sub/"), completion("cd sub/", &[]));
        // Absolute paths too.
        let line = format!("cd {}/subw", dir.path().display());
        let completed = format!("cd {}/subway/", dir.path().display());
        assert_eq!(i.complete(&line).line, completed);
        // Nothing after the last argument.
        assert_eq!(i.complete("cd sub ").candidates, [""; 0]);
    }

    #[test]
    fn completes_subcommands_and_custom_values() {
        let (i, _dir) = setup();
        assert_eq!(i.complete("git l"), completion("git log ", &[]));
        assert_eq!(i.complete("git "), completion("git ", &["log", "status"]));
        assert_eq!(i.complete("git log e"), completion("git log echo ", &[]));
        assert_eq!(i.complete("git frob "), completion("git frob ", &[]));
    }

    #[test]
    fn commands_without_a_spec_are_not_completed() {
        let (i, _dir) = setup();
        assert_eq!(i.complete("echo no"), completion("echo no", &[]));
        assert_eq!(i.complete("nope no"), completion("nope no", &[]));
    }

    #[test]
    fn quoted_command_names() {
        let (i, _dir) = setup();
        assert_eq!(i.complete("'ec"), completion("'echo' ", &[]));
        assert_eq!(i.complete("'ls' no"), completion("'ls' notes.txt ", &[]));
    }
}
