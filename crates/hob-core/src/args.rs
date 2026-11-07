//! Declared arguments: what a command takes, for parsing, usage and help,
//! and completion.
//!
//! A [`Spec`] lists a command's options, then either its positional
//! arguments or its subcommands. [`Interpreter::register_spec`] registers a
//! command with one; the command then gets [`Matches`] instead of words.
//!
//! Options are parsed like `getopt`: they come before the first operand, `--`
//! ends them, short flags combine (`-al`), and an option's value follows it
//! (`-n 5`, `-n5`, `--max-count 5`, `--max-count=5`). A lone `-` is an operand.
//!
//! ```
//! use hob_core::args::{Kind, Spec};
//!
//! let spec = Spec::new()
//!     .flag("all", "Show hidden files")
//!     .short('a')
//!     .arg("PATH", Kind::Path)
//!     .optional()
//!     .repeated();
//! let args = ["-a".to_owned(), "src".to_owned()];
//! let matches = spec.parse(&args, "ls").unwrap();
//! assert!(matches.flag("all"));
//! assert_eq!(matches.values("PATH"), ["src"]);
//! assert_eq!(spec.usage("ls"), "usage: ls [-a] [PATH...]");
//! ```
//!
//! [`Interpreter::register_spec`]: crate::Interpreter::register_spec

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

/// What a command takes: options, then positional arguments or subcommands.
#[derive(Clone, Default)]
pub struct Spec {
    options: Vec<Opt>,
    args: Vec<Arg>,
    subcommands: Vec<Subcommand>,
    /// What [`short`](Spec::short), [`optional`](Spec::optional) and
    /// [`repeated`](Spec::repeated) change.
    last: Option<Last>,
}

#[derive(Clone, Copy)]
enum Last {
    Opt,
    Arg,
}

#[derive(Clone)]
struct Opt {
    long: String,
    short: Option<char>,
    /// The value's name (e.g. `N`) and kind, if it takes one.
    value: Option<(String, Kind)>,
    help: String,
}

impl Opt {
    /// How usage shows it: `-a`, or `--long` if it has no short name.
    fn name(&self) -> String {
        match self.short {
            Some(short) => format!("-{short}"),
            None => format!("--{}", self.long),
        }
    }
}

#[derive(Clone)]
struct Arg {
    name: String,
    kind: Kind,
    optional: bool,
    repeated: bool,
}

#[derive(Clone)]
struct Subcommand {
    name: String,
    description: String,
    spec: Spec,
}

/// What kind of value an argument is, for completion.
#[derive(Clone)]
pub enum Kind {
    /// Anything; not completed.
    Text,
    /// A path to a file or directory.
    Path,
    /// A path to a directory.
    Dir,
    /// One of these words; others are an error.
    Choice(&'static [&'static str]),
    /// Anything; completed with what the function returns.
    Custom(Completer),
}

impl Kind {
    /// A kind completed with what `complete` returns (all candidates; those
    /// that don't fit what's typed are left out).
    pub fn custom(
        complete: impl Fn(&CompletionContext) -> Vec<String> + Send + Sync + 'static,
    ) -> Self {
        Kind::Custom(Completer(Arc::new(complete)))
    }
}

/// A function that lists candidates for a [`Kind::Custom`] argument.
#[derive(Clone)]
pub struct Completer(Arc<CompleteFn>);

type CompleteFn = dyn Fn(&CompletionContext) -> Vec<String> + Send + Sync;

impl Completer {
    /// Lists the candidates.
    pub fn complete(&self, ctx: &CompletionContext) -> Vec<String> {
        (self.0)(ctx)
    }
}

impl fmt::Debug for Completer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Completer")
    }
}

/// What a [`Completer`] can look at.
pub struct CompletionContext<'a> {
    pub(crate) cwd: &'a Path,
    pub(crate) commands: Vec<&'a str>,
}

impl CompletionContext<'_> {
    /// The interpreter's working directory.
    pub fn cwd(&self) -> &Path {
        self.cwd
    }

    /// The names (or prefixes) of all commands, sorted.
    pub fn commands(&self) -> &[&str] {
        &self.commands
    }
}

impl Spec {
    /// A spec that takes nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an option without a value, `--long`. The help says in a few
    /// words what it does.
    pub fn flag(self, long: &str, help: &str) -> Self {
        self.push_opt(long, None, help)
    }

    /// Adds an option with a value, `--long VALUE`.
    pub fn option(self, long: &str, value: &str, kind: Kind, help: &str) -> Self {
        self.push_opt(long, Some((value.to_owned(), kind)), help)
    }

    fn push_opt(mut self, long: &str, value: Option<(String, Kind)>, help: &str) -> Self {
        assert!(
            !long.is_empty() && !long.starts_with('-') && !long.contains(['=', ' ']),
            "bad option name {long:?}"
        );
        assert!(
            self.options.iter().all(|o| o.long != long),
            "option --{long} is declared twice"
        );
        self.options.push(Opt {
            long: long.to_owned(),
            short: None,
            value,
            help: help.to_owned(),
        });
        self.last = Some(Last::Opt);
        self
    }

    /// Gives the option just added a short name, `-c`.
    ///
    /// # Panics
    ///
    /// If the last thing added isn't an option, or the name is taken.
    pub fn short(mut self, short: char) -> Self {
        assert!(
            matches!(self.last, Some(Last::Opt)),
            "short() follows an option"
        );
        assert!(
            short.is_alphanumeric() && self.options.iter().all(|o| o.short != Some(short)),
            "bad or repeated short option -{short}"
        );
        self.options.last_mut().expect("an option").short = Some(short);
        self
    }

    /// Adds a positional argument, required unless made
    /// [`optional`](Spec::optional). Its name is shown in usage, e.g. `FILE`.
    ///
    /// # Panics
    ///
    /// If the spec has subcommands, or an earlier argument is repeated.
    pub fn arg(mut self, name: &str, kind: Kind) -> Self {
        assert!(
            self.subcommands.is_empty(),
            "a spec has args or subcommands"
        );
        assert!(
            self.args.last().is_none_or(|a| !a.repeated),
            "only the last argument can be repeated"
        );
        self.args.push(Arg {
            name: name.to_owned(),
            kind,
            optional: false,
            repeated: false,
        });
        self.last = Some(Last::Arg);
        self
    }

    /// Makes the argument just added optional. Arguments after it must be
    /// optional too.
    pub fn optional(mut self) -> Self {
        self.last_arg().optional = true;
        self
    }

    /// Makes the argument just added take any number of words (at least one,
    /// unless it's also optional). It must be the last.
    pub fn repeated(mut self) -> Self {
        self.last_arg().repeated = true;
        self
    }

    fn last_arg(&mut self) -> &mut Arg {
        assert!(matches!(self.last, Some(Last::Arg)), "follows an argument");
        self.args.last_mut().expect("an argument")
    }

    /// Adds a subcommand: the first operand picks one, and the rest is
    /// parsed by its spec.
    ///
    /// # Panics
    ///
    /// If the spec has positional arguments.
    pub fn subcommand(mut self, name: &str, description: &str, spec: Spec) -> Self {
        assert!(self.args.is_empty(), "a spec has args or subcommands");
        self.subcommands.push(Subcommand {
            name: name.to_owned(),
            description: description.to_owned(),
            spec,
        });
        self.last = None;
        self
    }

    /// Parses a command's arguments. `path` is the command's name (with any
    /// subcommands before it), for the usage in errors.
    pub fn parse(&self, words: &[String], path: &str) -> Result<Matches, String> {
        self.parse_inner(words, path).map_err(|e| match e {
            ParseError::Here(message) => format!("{message}; {}", self.usage(path)),
            ParseError::Done(message) => message,
        })
    }

    fn parse_inner(&self, words: &[String], path: &str) -> Result<Matches, ParseError> {
        let mut matches = Matches::new(self);
        let mut operands = Vec::new();
        let mut options_done = false;
        let mut words = words.iter();
        while let Some(word) = words.next() {
            if options_done || !operands.is_empty() || word == "-" || !word.starts_with('-') {
                operands.push(word.clone());
                if !self.subcommands.is_empty() {
                    break;
                }
                continue;
            }
            if word == "--" {
                options_done = true;
                continue;
            }
            if let Some(long) = word.strip_prefix("--") {
                let (long, inline) = match long.split_once('=') {
                    Some((long, value)) => (long, Some(value)),
                    None => (long, None),
                };
                let opt = (self.options.iter())
                    .find(|o| o.long == long)
                    .ok_or_else(|| here(format!("unknown option --{long}")))?;
                match (&opt.value, inline) {
                    (None, None) => matches.set_flag(opt),
                    (None, Some(_)) => {
                        return Err(here(format!("option --{long} takes no value")));
                    }
                    (Some(_), Some(value)) => matches.set_value(opt, value)?,
                    (Some(_), None) => {
                        let value = words.next().ok_or_else(|| needs_value(opt))?;
                        matches.set_value(opt, value)?;
                    }
                }
                continue;
            }
            let letters = &word[1..];
            for (i, letter) in letters.char_indices() {
                let opt = (self.options.iter())
                    .find(|o| o.short == Some(letter))
                    .ok_or_else(|| here(format!("unknown option -{letter}")))?;
                if opt.value.is_none() {
                    matches.set_flag(opt);
                    continue;
                }
                let rest = &letters[i + letter.len_utf8()..];
                let value = match rest.is_empty() {
                    false => rest,
                    true => words.next().ok_or_else(|| needs_value(opt))?,
                };
                matches.set_value(opt, value)?;
                break;
            }
        }
        if !self.subcommands.is_empty() {
            let Some(name) = operands.first() else {
                return Err(here("missing subcommand".to_owned()));
            };
            let sub = (self.subcommands.iter())
                .find(|s| &s.name == name)
                .ok_or_else(|| here(format!("unknown subcommand {name:?}")))?;
            let rest: Vec<String> = words.cloned().collect();
            let path = format!("{path} {name}");
            let inner = sub.spec.parse(&rest, &path).map_err(ParseError::Done)?;
            matches.subcommand = Some((name.clone(), Box::new(inner)));
            return Ok(matches);
        }
        let mut operands = operands.into_iter().peekable();
        for arg in &self.args {
            let mut values = Vec::new();
            while let Some(operand) = operands.next_if(|_| arg.repeated || values.is_empty()) {
                check_choice(&arg.kind, &arg.name, &operand)?;
                values.push(operand);
            }
            if values.is_empty() && !arg.optional {
                return Err(here(format!("missing {}", arg.name)));
            }
            matches.values.insert(arg.name.clone(), values);
        }
        match operands.next() {
            Some(extra) => Err(here(format!("unexpected argument {extra:?}"))),
            None => Ok(matches),
        }
    }

    /// The usage line, e.g. `usage: ls [-a] [PATH...]`.
    pub fn usage(&self, path: &str) -> String {
        format!("usage: {path}{}", self.synopsis())
    }

    /// What follows the command's name in usage, with a leading space.
    fn synopsis(&self) -> String {
        let mut s = String::new();
        for opt in &self.options {
            match &opt.value {
                Some((value, _)) => s += &format!(" [{} {value}]", opt.name()),
                None => s += &format!(" [{}]", opt.name()),
            }
        }
        for arg in &self.args {
            let dots = if arg.repeated { "..." } else { "" };
            match arg.optional {
                true => s += &format!(" [{}{dots}]", arg.name),
                false => s += &format!(" {}{dots}", arg.name),
            }
        }
        let subs: Vec<String> = (self.subcommands.iter())
            .map(|sub| format!("{}{}", sub.name, sub.spec.synopsis()))
            .collect();
        if !subs.is_empty() {
            s += " ";
            s += &subs.join(" | ");
        }
        s
    }

    /// Full help: the usage, then a line for each option and subcommand
    /// (with the options of subcommands below them).
    pub fn help(&self, path: &str) -> String {
        let mut lines = vec![self.usage(path)];
        self.help_lines("  ", &mut lines);
        lines.join("\n")
    }

    fn help_lines(&self, indent: &str, lines: &mut Vec<String>) {
        for opt in &self.options {
            let names = match opt.short {
                Some(short) => format!("-{short}, --{}", opt.long),
                None => format!("--{}", opt.long),
            };
            let value = match &opt.value {
                Some((value, _)) => format!(" {value}"),
                None => String::new(),
            };
            lines.push(format!("{indent}{names}{value} — {}", opt.help));
        }
        for sub in &self.subcommands {
            lines.push(format!(
                "{indent}{}{} — {}",
                sub.name,
                sub.spec.synopsis(),
                sub.description
            ));
            sub.spec.help_lines(&format!("{indent}  "), lines);
        }
    }
}

/// What can come next on a command line, for completion.
pub(crate) struct Expect<'a> {
    /// The long names of the options, if they're still allowed.
    pub options: Vec<&'a str>,
    /// The words that can come next: subcommand names, or the kind of an
    /// argument or option value.
    pub next: Next<'a>,
}

/// See [`Expect::next`].
pub(crate) enum Next<'a> {
    Subcommands(Vec<&'a str>),
    Value(&'a Kind),
    Nothing,
}

impl Spec {
    /// What can follow `words` (the finished arguments), leniently: unknown
    /// options are skipped. `None` if a subcommand is unknown.
    pub(crate) fn expect(&self, words: &[String]) -> Option<Expect<'_>> {
        let mut operands = 0;
        let mut options_done = false;
        let mut words = words.iter();
        while let Some(word) = words.next() {
            if options_done || operands > 0 || word == "-" || !word.starts_with('-') {
                if !self.subcommands.is_empty() {
                    let sub = self.subcommands.iter().find(|s| &s.name == word)?;
                    return sub.spec.expect(words.as_slice());
                }
                operands += 1;
                continue;
            }
            if word == "--" {
                options_done = true;
                continue;
            }
            // The option whose value is the next word, if any.
            let wants_value = match word.strip_prefix("--") {
                Some(long) if !long.contains('=') => {
                    value_of(self.options.iter().find(|o| o.long == long))
                }
                Some(_) => None,
                None => {
                    let letters = &word[1..];
                    letters
                        .char_indices()
                        .find_map(|(i, letter)| {
                            let opt = self.options.iter().find(|o| o.short == Some(letter));
                            let rest = &letters[i + letter.len_utf8()..];
                            value_of(opt).map(|kind| rest.is_empty().then_some(kind))
                        })
                        .flatten()
                }
            };
            if let Some(kind) = wants_value
                && words.next().is_none()
            {
                return Some(Expect {
                    options: Vec::new(),
                    next: Next::Value(kind),
                });
            }
        }
        let options = match options_done || operands > 0 {
            true => Vec::new(),
            false => self.options.iter().map(|o| o.long.as_str()).collect(),
        };
        let next = if !self.subcommands.is_empty() {
            Next::Subcommands(self.subcommands.iter().map(|s| s.name.as_str()).collect())
        } else {
            // Each argument takes one operand, except a repeated last one.
            let arg = (self.args.iter().enumerate())
                .find(|&(i, arg)| i == operands || (arg.repeated && i < operands));
            match arg {
                Some((_, arg)) => Next::Value(&arg.kind),
                None => Next::Nothing,
            }
        };
        Some(Expect { options, next })
    }
}

/// The kind of an option's value, if it has one.
fn value_of(opt: Option<&Opt>) -> Option<&Kind> {
    opt.and_then(|o| o.value.as_ref().map(|(_, kind)| kind))
}

/// A parse error: one to add this spec's usage to, or a finished message
/// (from a subcommand, with its usage).
enum ParseError {
    Here(String),
    Done(String),
}

fn here(message: String) -> ParseError {
    ParseError::Here(message)
}

fn needs_value(opt: &Opt) -> ParseError {
    let value = opt.value.as_ref().map_or("", |(value, _)| value);
    here(format!("option {} needs a value {value}", opt.name()))
}

fn check_choice(kind: &Kind, name: &str, value: &str) -> Result<(), ParseError> {
    match kind {
        Kind::Choice(choices) if !choices.contains(&value) => Err(here(format!(
            "invalid {name} {value:?} (expected {})",
            choices.join(", ")
        ))),
        _ => Ok(()),
    }
}

/// A command's parsed arguments. Options are named by their long name,
/// positional arguments by their name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Matches {
    flags: BTreeMap<String, bool>,
    values: BTreeMap<String, Vec<String>>,
    /// The value names of options (e.g. `N`), by long name, for errors.
    value_names: BTreeMap<String, String>,
    subcommand: Option<(String, Box<Matches>)>,
}

impl Matches {
    /// Matches with nothing given yet. Every name of the spec has an entry,
    /// so asking about another name is caught.
    fn new(spec: &Spec) -> Self {
        let mut matches = Matches::default();
        for opt in &spec.options {
            if let Some((value_name, _)) = &opt.value {
                matches.values.insert(opt.long.clone(), Vec::new());
                (matches.value_names).insert(opt.long.clone(), value_name.clone());
            } else {
                matches.flags.insert(opt.long.clone(), false);
            }
        }
        matches
    }

    fn set_flag(&mut self, opt: &Opt) {
        self.flags.insert(opt.long.clone(), true);
    }

    fn set_value(&mut self, opt: &Opt, value: &str) -> Result<(), ParseError> {
        let (name, kind) = opt.value.as_ref().expect("the option takes a value");
        check_choice(kind, name, value)?;
        let values = self.values.get_mut(&opt.long).expect("declared");
        values.push(value.to_owned());
        Ok(())
    }

    /// Whether a flag was given.
    ///
    /// # Panics
    ///
    /// If the spec has no such flag.
    pub fn flag(&self, long: &str) -> bool {
        *(self.flags.get(long)).unwrap_or_else(|| panic!("no flag --{long} declared"))
    }

    /// The value of an option or argument, if given (an option's last one, if
    /// given several times).
    ///
    /// # Panics
    ///
    /// If the spec has no such option or argument.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values(name).last().map(String::as_str)
    }

    /// All values of an option or argument, in order.
    ///
    /// # Panics
    ///
    /// If the spec has no such option or argument.
    pub fn values(&self, name: &str) -> &[String] {
        (self.values.get(name)).unwrap_or_else(|| panic!("no option or argument {name} declared"))
    }

    /// The [`value`](Matches::value) parsed with `FromStr`, if given.
    pub fn parse<T: FromStr>(&self, name: &str) -> Result<Option<T>, String> {
        let shown = self.value_names.get(name).map_or(name, String::as_str);
        self.value(name)
            .map(|value| {
                value
                    .parse()
                    .map_err(|_| format!("invalid {shown}: {value:?}"))
            })
            .transpose()
    }

    /// The subcommand given, and its matches. A spec with subcommands always
    /// has one.
    pub fn subcommand(&self) -> Option<(&str, &Matches)> {
        (self.subcommand.as_ref()).map(|(name, matches)| (name.as_str(), &**matches))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    fn ls() -> Spec {
        Spec::new()
            .flag("all", "Show hidden files")
            .short('a')
            .flag("long", "Show details")
            .short('l')
            .option("width", "N", Kind::Text, "Lines are N wide")
            .short('w')
            .option(
                "color",
                "WHEN",
                Kind::Choice(&["always", "never"]),
                "Use colours",
            )
            .arg("PATH", Kind::Path)
            .optional()
            .repeated()
    }

    fn parse(spec: &Spec, line: &str) -> Result<Matches, String> {
        spec.parse(&words(line), "ls")
    }

    #[test]
    fn flags_combine() {
        let m = parse(&ls(), "-al x").unwrap();
        assert!(m.flag("all") && m.flag("long"));
        assert_eq!(m.values("PATH"), ["x"]);
        let m = parse(&ls(), "--all").unwrap();
        assert!(m.flag("all") && !m.flag("long"));
        assert_eq!(m.values("PATH"), [""; 0]);
    }

    #[test]
    fn options_take_values() {
        for line in ["-w 5", "-w5", "-aw5", "--width 5", "--width=5", "-w 3 -w 5"] {
            let m = parse(&ls(), line).unwrap();
            assert_eq!(m.parse::<u32>("width"), Ok(Some(5)), "{line}");
        }
        assert_eq!(parse(&ls(), "").unwrap().value("width"), None);
        let m = parse(&ls(), "-w x").unwrap();
        assert_eq!(m.parse::<u32>("width"), Err("invalid N: \"x\"".into()));
    }

    #[test]
    fn options_end_at_the_first_operand() {
        let m = parse(&ls(), "a -l").unwrap();
        assert!(!m.flag("long"));
        assert_eq!(m.values("PATH"), ["a", "-l"]);
        let m = parse(&ls(), "-a -- -l").unwrap();
        assert_eq!(m.values("PATH"), ["-l"]);
        let m = parse(&ls(), "- -l").unwrap();
        assert_eq!(m.values("PATH"), ["-", "-l"]);
    }

    #[test]
    fn errors_show_usage() {
        let usage = "usage: ls [-a] [-l] [-w N] [--color WHEN] [PATH...]";
        let error = |line| parse(&ls(), line).unwrap_err();
        assert_eq!(error("-x"), format!("unknown option -x; {usage}"));
        assert_eq!(error("--x"), format!("unknown option --x; {usage}"));
        assert_eq!(error("-w"), format!("option -w needs a value N; {usage}"));
        assert_eq!(
            error("--all=1"),
            format!("option --all takes no value; {usage}")
        );
        assert_eq!(
            error("--color=x"),
            format!("invalid WHEN \"x\" (expected always, never); {usage}")
        );
    }

    #[test]
    fn counts_are_checked() {
        let spec = Spec::new()
            .arg("A", Kind::Text)
            .arg("B", Kind::Text)
            .optional();
        let m = spec.parse(&words("1"), "x").unwrap();
        assert_eq!((m.value("A"), m.value("B")), (Some("1"), None));
        let m = spec.parse(&words("1 2"), "x").unwrap();
        assert_eq!(m.value("B"), Some("2"));
        assert_eq!(
            spec.parse(&words(""), "x").unwrap_err(),
            "missing A; usage: x A [B]"
        );
        assert_eq!(
            spec.parse(&words("1 2 3"), "x").unwrap_err(),
            "unexpected argument \"3\"; usage: x A [B]"
        );
        let spec = Spec::new().arg("FILE", Kind::Path).repeated();
        assert_eq!(spec.usage("cat"), "usage: cat FILE...");
        assert!(spec.parse(&words(""), "cat").is_err());
        assert_eq!(
            spec.parse(&words("a b"), "cat").unwrap().values("FILE"),
            ["a", "b"]
        );
        assert_eq!(
            Spec::new().parse(&words("a"), "pwd").unwrap_err(),
            "unexpected argument \"a\"; usage: pwd"
        );
    }

    fn git() -> Spec {
        Spec::new()
            .subcommand("status", "Show the status", Spec::new())
            .subcommand(
                "log",
                "Show commits",
                Spec::new()
                    .option("max-count", "N", Kind::Text, "Show N commits")
                    .short('n')
                    .arg("REV", Kind::Text)
                    .optional(),
            )
    }

    #[test]
    fn subcommands() {
        let m = git().parse(&words("log -n 3 main"), "git").unwrap();
        let (name, sub) = m.subcommand().unwrap();
        assert_eq!(name, "log");
        assert_eq!(
            (sub.value("max-count"), sub.value("REV")),
            (Some("3"), Some("main"))
        );
        let usage = "usage: git status | log [-n N] [REV]";
        assert_eq!(git().usage("git"), usage);
        let error = |line| git().parse(&words(line), "git").unwrap_err();
        assert_eq!(error(""), format!("missing subcommand; {usage}"));
        assert_eq!(
            error("frob"),
            format!("unknown subcommand \"frob\"; {usage}")
        );
        assert_eq!(
            error("log -x"),
            "unknown option -x; usage: git log [-n N] [REV]"
        );
    }

    #[test]
    fn help_lists_options_and_subcommands() {
        assert_eq!(
            ls().help("ls"),
            "usage: ls [-a] [-l] [-w N] [--color WHEN] [PATH...]\n\
             \x20 -a, --all — Show hidden files\n\
             \x20 -l, --long — Show details\n\
             \x20 -w, --width N — Lines are N wide\n\
             \x20 --color WHEN — Use colours"
        );
        assert_eq!(
            git().help("git"),
            "usage: git status | log [-n N] [REV]\n\
             \x20 status — Show the status\n\
             \x20 log [-n N] [REV] — Show commits\n\
             \x20   -n, --max-count N — Show N commits"
        );
    }

    #[test]
    #[should_panic(expected = "no flag --nope declared")]
    fn undeclared_names_panic() {
        parse(&ls(), "").unwrap().flag("nope");
    }
}
