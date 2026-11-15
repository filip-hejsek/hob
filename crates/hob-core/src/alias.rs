//! Aliases: names that stand for the start of a command line, like a
//! shell's. When a line's first word is an alias (unquoted), it's replaced
//! by the alias's line; the rest of the line follows. The new first word
//! may be an alias too, except one already expanded, so an alias can
//! extend the command of the same name (`ls` = `ls -a`).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::complete::Completion;
use crate::line;

/// The aliases: their lines, by name.
pub(crate) type Aliases = Arc<BTreeMap<String, String>>;

/// An alias for the start of a command line, e.g. `ll` for `ls -l`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alias {
    name: String,
    line: String,
}

impl Alias {
    /// An alias called `name` for `line`, if both are valid (see
    /// [`check_name`](Self::check_name) and
    /// [`check_line`](Self::check_line)).
    pub fn new(name: impl Into<String>, line: impl Into<String>) -> Result<Self, AliasError> {
        let (name, line) = (name.into(), line.into());
        Self::check_name(&name)?;
        Self::check_line(&line)?;
        Ok(Alias { name, line })
    }

    /// Checks an alias's name: like a command's, it must start with a
    /// letter or digit, and it can't have whitespace, quotes or
    /// backslashes.
    pub fn check_name(name: &str) -> Result<(), AliasError> {
        if !name.starts_with(char::is_alphanumeric) {
            return Err(AliasError(format!(
                "the alias name `{name}` must start with a letter or digit"
            )));
        }
        if name.contains(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '\\')) {
            return Err(AliasError(format!(
                "the alias name `{name}` can't have whitespace, quotes or backslashes"
            )));
        }
        Ok(())
    }

    /// Checks the line an alias stands for: it can't be empty, and unless
    /// it runs a prefix command (it starts with a special character), it
    /// must split into words (no unclosed quote).
    pub fn check_line(line: &str) -> Result<(), AliasError> {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            return Err(AliasError("an alias can't be empty".to_owned()));
        }
        let prefix = trimmed
            .starts_with(|c: char| !c.is_alphanumeric() && !matches!(c, '\'' | '"' | '\\' | '~'));
        if !prefix && let Err(e) = line::split(line) {
            return Err(AliasError(format!("{e} in `{line}`")));
        }
        Ok(())
    }

    /// The alias's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The line it stands for.
    pub fn line(&self) -> &str {
        &self.line
    }

    pub(crate) fn into_parts(self) -> (String, String) {
        (self.name, self.line)
    }
}

/// Why an [`Alias`] isn't valid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AliasError(String);

impl fmt::Display for AliasError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AliasError {}

/// A line with its aliases expanded: `line[..to]` stands for the first
/// `from` bytes of the original; the rest is the same.
pub(crate) struct Expansion {
    pub(crate) line: String,
    pub(crate) from: usize,
    pub(crate) to: usize,
}

impl Expansion {
    /// Turns a completion of the expanded line back into one of
    /// `original`, the line that was expanded.
    pub(crate) fn unexpand(&self, completion: Completion, original: &str) -> Completion {
        let head = &self.line[..self.to];
        let back = |s: String| match s.strip_prefix(head) {
            Some(rest) => format!("{}{rest}", &original[..self.from]),
            None => s,
        };
        let choice = |mut c: crate::Choice| {
            c.bare = back(c.bare);
            c
        };
        Completion {
            line: back(completion.line),
            choices: completion.choices.into_iter().map(choice).collect(),
            only: completion.only.map(choice),
        }
    }
}

/// Expands the aliases at the start of `line`, if it starts with one.
pub(crate) fn expand(aliases: &Aliases, line: &str) -> Option<Expansion> {
    let mut expanded: Option<String> = None;
    let mut from = 0;
    let mut used: Vec<&str> = Vec::new();
    loop {
        let current = expanded.as_deref().unwrap_or(line);
        let trimmed = current.trim_start();
        let start = current.len() - trimmed.len();
        let end = start + trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        // Names have no quotes or backslashes, so a word that matches one
        // is unquoted.
        let Some((name, replacement)) = aliases.get_key_value(&current[start..end]) else {
            break;
        };
        if used.contains(&name.as_str()) {
            break;
        }
        used.push(name);
        if expanded.is_none() {
            from = end;
        }
        expanded = Some(format!("{replacement}{}", &current[end..]));
    }
    let line_len = line.len();
    expanded.map(|expanded| Expansion {
        to: expanded.len() - (line_len - from),
        line: expanded,
        from,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aliases(pairs: &[(&str, &str)]) -> Aliases {
        Arc::new(
            (pairs.iter())
                .map(|&(n, l)| (n.to_owned(), l.to_owned()))
                .collect(),
        )
    }

    fn expanded(aliases: &Aliases, line: &str) -> Option<String> {
        expand(aliases, line).map(|e| e.line)
    }

    #[test]
    fn the_first_word_is_replaced() {
        let a = aliases(&[("ll", "ls -l"), ("g", "git")]);
        assert_eq!(expanded(&a, "ll"), Some("ls -l".into()));
        assert_eq!(expanded(&a, "  ll  foo"), Some("ls -l  foo".into()));
        assert_eq!(expanded(&a, "ls ll"), None);
        assert_eq!(expanded(&a, "lll"), None);
        assert_eq!(expanded(&a, "'ll'"), None, "quoted");
        assert_eq!(expanded(&a, "\\ll"), None, "escaped");
        assert_eq!(expanded(&a, ""), None);
    }

    #[test]
    fn expansions_are_expanded_but_not_again() {
        let a = aliases(&[("ls", "ls -a"), ("ll", "ls -l"), ("a", "b"), ("b", "a x")]);
        assert_eq!(expanded(&a, "ll y"), Some("ls -a -l y".into()));
        assert_eq!(expanded(&a, "ls"), Some("ls -a".into()));
        assert_eq!(expanded(&a, "a"), Some("a x".into()));
    }

    #[test]
    fn the_rest_keeps_its_place() {
        let a = aliases(&[("ll", "ls -l"), ("l", "ll")]);
        let e = expand(&a, " l foo").unwrap();
        assert_eq!(e.line, "ls -l foo");
        assert_eq!((e.from, e.to), (2, 5));
    }

    #[test]
    fn alias_names_and_lines_are_checked() {
        assert!(Alias::new("ll", "ls -l").is_ok());
        assert!(Alias::new("sh", "!sh -c 'x").is_ok(), "a prefix line");
        assert!(Alias::new("!x", "ls").is_err());
        assert!(Alias::new("", "ls").is_err());
        assert!(Alias::new("a b", "ls").is_err());
        assert!(Alias::new("a'", "ls").is_err());
        assert!(Alias::new("x", " ").is_err());
        assert_eq!(
            Alias::new("x", "echo 'a").unwrap_err().to_string(),
            "missing closing ' in `echo 'a`"
        );
    }
}
