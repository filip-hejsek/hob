//! Splitting command lines into words, with quoting like a POSIX shell's.
//!
//! `'…'` is literal; in `"…"`, `\"` and `\\` are escapes (any other `\` is
//! kept); outside quotes, `\` escapes any character. Pieces next to each
//! other join into one word (`a"b c"d`). An unquoted `~` at the start of a
//! word, alone or before `/`, is the home directory.

use std::fmt;

/// A word of a command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Word {
    /// The word with quotes and escapes removed, and `~` expanded.
    pub value: String,
    /// Where the word starts in the line, in bytes.
    pub start: usize,
    /// Where it ends (after its last character, including a closing quote).
    pub end: usize,
}

/// Why a line couldn't be split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SyntaxError {
    /// A quote (`'` or `"`) isn't closed.
    UnterminatedQuote(char),
    /// The line ends with a backslash, which has nothing to escape.
    TrailingBackslash,
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyntaxError::UnterminatedQuote(quote) => write!(f, "missing closing {quote}"),
            SyntaxError::TrailingBackslash => f.write_str("the line ends with a backslash"),
        }
    }
}

/// What [`scan`] found: the words, and how the line ended.
struct Scan {
    words: Vec<Word>,
    /// The quote the line ends inside of, if any.
    quote: Option<char>,
    /// Whether the line ends with an unquoted backslash.
    escape: bool,
}

/// Splits a line into words, expanding `~` to `home` (if known).
fn scan(line: &str, home: Option<&str>) -> Scan {
    let mut words = Vec::new();
    let mut word: Option<Word> = None;
    let mut quote = None;
    let mut escape = false;
    let mut chars = line.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let end = i + c.len_utf8();
        let current = word.get_or_insert_with(|| Word {
            value: String::new(),
            start: i,
            end,
        });
        current.end = end;
        match quote {
            Some('\'') if c == '\'' => quote = None,
            Some('\'') => current.value.push(c),
            Some(_) => match c {
                '"' => quote = None,
                '\\' => match chars.peek() {
                    Some(&(j, next @ ('"' | '\\'))) => {
                        chars.next();
                        current.value.push(next);
                        current.end = j + 1;
                    }
                    _ => current.value.push('\\'),
                },
                _ => current.value.push(c),
            },
            None => match c {
                c if c.is_whitespace() => {
                    // The word ended before this character.
                    let mut done = word.take().expect("a word was started");
                    if done.start < i {
                        done.end = i;
                        words.push(done);
                    }
                }
                '\'' | '"' => quote = Some(c),
                '\\' => match chars.next() {
                    Some((j, next)) => {
                        current.value.push(next);
                        current.end = j + next.len_utf8();
                    }
                    None => escape = true,
                },
                '~' if current.start == i
                    && chars
                        .peek()
                        .is_none_or(|&(_, n)| n == '/' || n.is_whitespace()) =>
                {
                    current.value.push_str(home.unwrap_or("~"));
                }
                _ => current.value.push(c),
            },
        }
    }
    words.extend(word);
    Scan {
        words,
        quote,
        escape,
    }
}

/// The home directory as a string, if it's known and valid UTF-8.
fn home() -> Option<String> {
    std::env::home_dir()?.into_os_string().into_string().ok()
}

/// Splits a line into words.
pub(crate) fn split(line: &str) -> Result<Vec<Word>, SyntaxError> {
    split_with_home(line, home().as_deref())
}

fn split_with_home(line: &str, home: Option<&str>) -> Result<Vec<Word>, SyntaxError> {
    let scan = scan(line, home);
    match (scan.quote, scan.escape) {
        (Some(quote), _) => Err(SyntaxError::UnterminatedQuote(quote)),
        (None, true) => Err(SyntaxError::TrailingBackslash),
        (None, false) => Ok(scan.words),
    }
}

/// The line without trailing whitespace, unless the whitespace is part of
/// a word (escaped or quoted).
pub fn trim_line_end(line: &str) -> &str {
    let scan = scan(line, None);
    match scan.words.last() {
        Some(last) if scan.quote.is_none() => &line[..last.end],
        Some(_) => line,
        None => line.trim_end(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        let words = split_with_home(line, Some("/home/me")).unwrap();
        words.into_iter().map(|w| w.value).collect()
    }

    #[test]
    fn splits_at_whitespace() {
        assert_eq!(words("  a  b\tc "), ["a", "b", "c"]);
        assert_eq!(words(""), [""; 0]);
        assert_eq!(words("   "), [""; 0]);
    }

    #[test]
    fn single_quotes_are_literal() {
        assert_eq!(words(r#"'a b' 'c\d"e'"#), ["a b", r#"c\d"e"#]);
    }

    #[test]
    fn double_quotes_escape_only_quotes_and_backslashes() {
        assert_eq!(words(r#""a \"b\" \\ \n 'c'""#), [r#"a "b" \ \n 'c'"#]);
    }

    #[test]
    fn backslashes_escape_outside_quotes() {
        assert_eq!(words(r"a\ b \'c \\ \n"), ["a b", "'c", "\\", "n"]);
    }

    #[test]
    fn pieces_join_into_one_word() {
        assert_eq!(words(r#"a"b c"'d e'f"#), ["ab cd ef"]);
    }

    #[test]
    fn empty_quotes_are_a_word() {
        assert_eq!(words(r#"a '' """#), ["a", "", ""]);
    }

    #[test]
    fn unicode() {
        assert_eq!(words("čaj 'ž ř' \\é"), ["čaj", "ž ř", "é"]);
    }

    #[test]
    fn tilde_is_the_home_directory() {
        assert_eq!(words("~ ~/a"), ["/home/me", "/home/me/a"]);
        assert_eq!(
            words(r#"~x a~ '~' "~" \~ ~"/a""#),
            ["~x", "a~", "~", "~", "~", "~/a"]
        );
        let words = split_with_home("~ ~/a", None).unwrap();
        assert_eq!(words[1].value, "~/a");
    }

    #[test]
    fn positions_cover_the_raw_words() {
        let line = r#" ab "c d"  e\ f"#;
        let spans: Vec<&str> = (split_with_home(line, None).unwrap().iter())
            .map(|w| &line[w.start..w.end])
            .collect();
        assert_eq!(spans, ["ab", r#""c d""#, r"e\ f"]);
    }

    #[test]
    fn errors() {
        let error = |line| split_with_home(line, None).unwrap_err();
        assert_eq!(error("a 'b"), SyntaxError::UnterminatedQuote('\''));
        assert_eq!(error(r#"a "b\""#), SyntaxError::UnterminatedQuote('"'));
        assert_eq!(error(r"a \"), SyntaxError::TrailingBackslash);
    }

    #[test]
    fn trims_only_whitespace_outside_words() {
        assert_eq!(trim_line_end(" a b  "), " a b");
        assert_eq!(trim_line_end(r"a\  "), r"a\ ");
        assert_eq!(trim_line_end("a 'b  "), "a 'b  ");
        assert_eq!(trim_line_end("a 'b ' "), "a 'b '");
        assert_eq!(trim_line_end("   "), "");
    }
}
