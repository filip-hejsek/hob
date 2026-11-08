//! Tab completion of command lines: command names, then whatever a
//! command's [`Spec`](crate::args::Spec) says comes next. The shell command
//! of a `!` line is completed with programs on `PATH`, then paths.

use std::ffi::OsStr;
use std::path::Path;
use std::{env, fs};

use crate::args::{CompletionContext, Expect, Kind, Next};
use crate::line::{self, Partial, Syntax, Word};
use crate::{Commands, Run, is_prefix, prefixed};

/// What [`Interpreter::complete`](crate::Interpreter::complete) made of a
/// line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    /// The line, completed as far as it's unambiguous.
    pub line: String,
    /// The possible completions, if there are several; otherwise empty.
    /// Paths are shown by their last component, like a shell does.
    pub candidates: Vec<String>,
    /// For each candidate, the line with it filled in as a finished word
    /// (its quote closed), but without a space after it: what a picker
    /// shows while the candidate is picked.
    pub filled: Vec<String>,
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

/// Completes `line`; programs are looked for in `path` (as in `PATH`).
pub(crate) fn complete(
    commands: &Commands,
    cwd: &Path,
    path: Option<&OsStr>,
    line: &str,
) -> Completion {
    let unchanged = Completion {
        line: line.to_owned(),
        candidates: Vec::new(),
        filled: Vec::new(),
    };
    let home = line::home();
    let trimmed = line.trim_start();
    let (syntax, words, partial) = match prefixed(commands, trimmed) {
        Some((prefix, command)) if matches!(command.run, Run::Rest { sh: true, .. }) => {
            let start = line.len() - trimmed.len() + prefix.len();
            let (words, mut partial) =
                line::split_partial(&line[start..], home.as_deref(), Syntax::Sh);
            partial.word.start += start;
            partial.word.end += start;
            (Syntax::Sh, words, partial)
        }
        Some(_) => return unchanged,
        None => {
            let (words, partial) = line::split_partial(line, home.as_deref(), Syntax::Hob);
            (Syntax::Hob, words, partial)
        }
    };
    let typed = partial.word.value.as_str();
    let candidates: Vec<Candidate> = match words.split_first() {
        _ if syntax == Syntax::Sh => sh_candidates(cwd, path, &words, typed),
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
            line: rewrite(
                line,
                &partial,
                &only.value,
                !only.open,
                home.as_deref(),
                syntax,
            ),
            candidates: Vec::new(),
            filled: Vec::new(),
        },
        [first, rest @ ..] => {
            let len = (rest.iter()).fold(first.value.len(), |len, c| {
                common_prefix_len(&first.value[..len], &c.value)
            });
            let common = &first.value[..len];
            let filled = (candidates.iter())
                .map(|c| {
                    let mut line = rewrite(line, &partial, &c.value, true, home.as_deref(), syntax);
                    line.pop(); // The space.
                    line
                })
                .collect();
            Completion {
                line: rewrite(line, &partial, common, false, home.as_deref(), syntax),
                candidates: candidates.into_iter().map(|c| c.shown).collect(),
                filled,
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
        Next::Value(Kind::Path) => return paths(ctx.cwd(), typed, Filter::All),
        Next::Value(Kind::Dir) => return paths(ctx.cwd(), typed, Filter::Dirs),
        Next::Subcommands(names) => names.iter().map(|&n| n.to_owned()).collect(),
        Next::Value(Kind::Choice(choices)) => choices.iter().map(|&c| c.to_owned()).collect(),
        Next::Value(Kind::Custom(completer)) => completer.complete(ctx),
    };
    let mut words: Vec<String> = words.into_iter().filter(|w| w.starts_with(typed)).collect();
    words.sort();
    words.dedup();
    words.iter().map(|w| Candidate::word(w)).collect()
}

/// The shell operators after which a new command starts.
const COMMAND_SEPARATORS: &[&str] = &["|", "||", "&&", ";", "&", "|&", "("];

/// The candidates for the word being typed in a shell command, after
/// `words`: a program where a command name goes (but not before anything is
/// typed: there would be too many), otherwise a path.
fn sh_candidates(cwd: &Path, path: Option<&OsStr>, words: &[Word], typed: &str) -> Vec<Candidate> {
    let is_command_name = (words.iter().rev())
        .take_while(|w| !(w.operator && COMMAND_SEPARATORS.contains(&w.value.as_str())))
        .all(|w| !w.operator && is_assignment(&w.value));
    if !is_command_name {
        paths(cwd, typed, Filter::All)
    } else if typed.contains('/') {
        paths(cwd, typed, Filter::Programs)
    } else if typed.is_empty() {
        Vec::new()
    } else {
        programs(cwd, path, typed)
    }
}

/// Whether a shell word is a variable assignment (`NAME=value`).
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The programs in `path` (like `PATH`) whose names start with `typed`, sorted. Hidden
/// ones only if `typed` starts with `.`. Relative directories in it are
/// relative to `cwd`, as for the shell.
fn programs(cwd: &Path, path: Option<&OsStr>, typed: &str) -> Vec<Candidate> {
    let Some(path) = path else {
        return Vec::new();
    };
    let mut names: Vec<String> = (env::split_paths(&path))
        .filter_map(|dir| fs::read_dir(cwd.join(dir)).ok())
        .flat_map(|entries| entries.flatten())
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let hidden = name.starts_with('.') && !typed.starts_with('.');
            (name.starts_with(typed) && !hidden && is_program(&entry.path())).then_some(name)
        })
        .collect();
    names.sort();
    names.dedup();
    names.iter().map(|name| Candidate::word(name)).collect()
}

/// Whether `path` is a file that can be run (following symlinks).
fn is_program(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    };
    #[cfg(not(unix))]
    let executable = true;
    metadata.is_file() && executable
}

/// Which paths are completed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Dirs,
    /// Directories and programs.
    Programs,
}

/// The paths that `typed` (relative to `cwd`) can be completed to: the
/// entries of its directory that start with its last component. Hidden ones
/// only if that starts with `.`. Directories end with `/`.
fn paths(cwd: &Path, typed: &str, filter: Filter) -> Vec<Candidate> {
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
            let path = entry.path();
            let is_dir = path.is_dir();
            let wanted = match filter {
                Filter::All => true,
                Filter::Dirs => is_dir,
                Filter::Programs => is_dir || is_program(&path),
            };
            if !wanted {
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
fn rewrite(
    line: &str,
    partial: &Partial,
    value: &str,
    done: bool,
    home: Option<&str>,
    syntax: Syntax,
) -> String {
    let word = line::requote(value, partial.quote, done, home, partial.word.tilde, syntax);
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
            filled: Vec::new(),
        }
    }

    /// Completes `line`, leaving out the filled-in lines, which
    /// [`fills_in_candidates`] checks.
    fn complete(i: &Interpreter, line: &str) -> Completion {
        Completion {
            filled: Vec::new(),
            ..i.complete(line)
        }
    }

    #[test]
    fn fills_in_candidates() {
        let (i, _dir) = setup();
        assert_eq!(i.complete("ls n").filled, [r"ls new\ file", "ls notes.txt"]);
        assert_eq!(
            i.complete("ls 'n").filled,
            ["ls 'new file'", "ls 'notes.txt'"]
        );
        assert_eq!(i.complete("cd s").filled, ["cd sub/", "cd subway/"]);
        assert_eq!(i.complete("ls --c").filled, [""; 0]);
    }

    #[test]
    fn completes_options() {
        let (i, _dir) = setup();
        assert_eq!(complete(&i, "ls --a"), completion("ls --all ", &[]));
        assert_eq!(
            complete(&i, "ls -"),
            completion("ls --", &["--all", "--color", "--width"])
        );
        // Not after an operand.
        assert_eq!(complete(&i, "ls notes.txt --a").candidates, [""; 0]);
    }

    #[test]
    fn completes_option_values() {
        let (i, _dir) = setup();
        assert_eq!(
            complete(&i, "ls --color a"),
            completion("ls --color a", &["always", "auto"])
        );
        assert_eq!(
            complete(&i, "ls --color n"),
            completion("ls --color never ", &[])
        );
        // A text value isn't completed, and isn't taken for a path.
        assert_eq!(complete(&i, "ls -aw n"), completion("ls -aw n", &[]));
        // Once given, the value isn't what's completed.
        assert_eq!(
            complete(&i, "ls -w5 no"),
            completion("ls -w5 notes.txt ", &[])
        );
    }

    #[test]
    fn completes_paths() {
        let (i, _dir) = setup();
        assert_eq!(complete(&i, "ls no"), completion("ls notes.txt ", &[]));
        assert_eq!(
            complete(&i, "ls n"),
            completion("ls n", &["new file", "notes.txt"])
        );
        assert_eq!(complete(&i, "ls ne"), completion(r"ls new\ file ", &[]));
        assert_eq!(complete(&i, "ls 'ne"), completion("ls 'new file' ", &[]));
        assert_eq!(
            complete(&i, r#"ls "new f"#),
            completion(r#"ls "new file" "#, &[])
        );
        // Directories get a slash and no space; their entries are listed by
        // name.
        assert_eq!(
            complete(&i, "ls sub"),
            completion("ls sub", &["sub/", "subway/"])
        );
        assert_eq!(
            complete(&i, "ls sub/"),
            completion("ls sub/inner.txt ", &[])
        );
        assert_eq!(complete(&i, "ls subw"), completion("ls subway/", &[]));
        // Hidden files only when asked for.
        assert_eq!(complete(&i, "ls .h"), completion("ls .hidden ", &[]));
        assert_eq!(complete(&i, "ls ").candidates.len(), 4);
        // Paths go on after the first.
        assert_eq!(complete(&i, "ls a no"), completion("ls a notes.txt ", &[]));
    }

    #[test]
    fn completes_directories() {
        let (i, dir) = setup();
        assert_eq!(
            complete(&i, "cd "),
            completion("cd sub", &["sub/", "subway/"])
        );
        assert_eq!(complete(&i, "cd sub/"), completion("cd sub/", &[]));
        // Absolute paths too.
        let line = format!("cd {}/subw", dir.path().display());
        let completed = format!("cd {}/subway/", dir.path().display());
        assert_eq!(complete(&i, &line).line, completed);
        // Nothing after the last argument.
        assert_eq!(complete(&i, "cd sub ").candidates, [""; 0]);
    }

    #[test]
    fn completes_subcommands_and_custom_values() {
        let (i, _dir) = setup();
        assert_eq!(complete(&i, "git l"), completion("git log ", &[]));
        assert_eq!(complete(&i, "git "), completion("git ", &["log", "status"]));
        assert_eq!(complete(&i, "git log e"), completion("git log echo ", &[]));
        assert_eq!(complete(&i, "git frob "), completion("git frob ", &[]));
    }

    #[test]
    fn commands_without_a_spec_are_not_completed() {
        let (i, _dir) = setup();
        assert_eq!(complete(&i, "echo no"), completion("echo no", &[]));
        assert_eq!(complete(&i, "nope no"), completion("nope no", &[]));
    }

    #[test]
    fn quoted_command_names() {
        let (i, _dir) = setup();
        assert_eq!(complete(&i, "'ec"), completion("'echo' ", &[]));
        assert_eq!(complete(&i, "'ls' no"), completion("'ls' notes.txt ", &[]));
    }

    /// Completes `line` in an interpreter with `!` in a temporary directory
    /// with some files, and `bin` and `sbin` directories as `PATH`.
    fn sh_filled(line: &str) -> Completion {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let files = [
            ("notes.txt", false),
            ("a$b.txt", false),
            ("run.sh", true),
            ("sub/x", false),
            ("bin/grep", true),
            ("bin/greet", true),
            ("bin/grok", false),
            ("bin/.gz", true),
            ("sbin/grep", true),
            ("sbin/gzip", true),
        ];
        for (file, executable) in files {
            let path = dir.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "").unwrap();
            let mode = if executable { 0o755 } else { 0o644 };
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        }
        let mut i = Interpreter::new();
        i.register_sh_prefix("!", "Test", |_, _| Ok(()));
        i.register_prefix("@", "Test", |_, _| Ok(()));
        let path = std::env::join_paths(["bin", "sbin"]).unwrap();
        super::complete(&i.commands, dir.path(), Some(&path), line)
    }

    /// Like [`sh_filled`], leaving out the filled-in lines.
    fn sh(line: &str) -> Completion {
        Completion {
            filled: Vec::new(),
            ..sh_filled(line)
        }
    }

    #[test]
    fn completes_programs_in_shell_commands() {
        assert_eq!(sh("!gr"), completion("!gre", &["greet", "grep"]));
        assert_eq!(sh("! gz"), completion("! gzip ", &[]));
        assert_eq!(sh("  !grep"), completion("  !grep ", &[]));
        assert_eq!(sh("!.g"), completion("!.gz ", &[]));
        // Not before anything is typed.
        assert_eq!(sh("!"), completion("!", &[]));
        // After operators, and assignments.
        assert_eq!(sh("!ls|gz"), completion("!ls|gzip ", &[]));
        assert_eq!(sh("!a && X=1 gz"), completion("!a && X=1 gzip ", &[]));
        // With a slash, programs and directories as paths.
        assert_eq!(
            sh("!./"),
            completion("!./", &["bin/", "run.sh", "sbin/", "sub/"])
        );
        assert_eq!(sh("!./r"), completion("!./run.sh ", &[]));
        // Other prefix commands aren't completed.
        assert_eq!(sh("@gz"), completion("@gz", &[]));
    }

    #[test]
    fn completes_paths_in_shell_commands() {
        assert_eq!(sh("!cat no"), completion("!cat notes.txt ", &[]));
        assert_eq!(sh("!cat gz no"), completion("!cat gz notes.txt ", &[]));
        assert_eq!(sh("!ls >no"), completion("!ls >notes.txt ", &[]));
        // Quoted for the shell.
        assert_eq!(sh("!cat a"), completion(r"!cat a\$b.txt ", &[]));
        assert_eq!(sh(r#"!cat "a"#), completion(r#"!cat "a\$b.txt" "#, &[]));
        assert_eq!(sh("!cat 'a"), completion("!cat 'a$b.txt' ", &[]));
        assert_eq!(
            sh_filled("!cat ").filled[..2],
            [r"!cat a\$b.txt", "!cat bin/"]
        );
    }
}
