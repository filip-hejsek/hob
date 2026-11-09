//! Tags: a note has at most one, written as the first word of its title, or
//! of its text when it has no title. A tag is stored apart from the title
//! and text, which don't keep it.

/// A tag a note can have. Each one means something.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tag {
    /// A TODO note: something to do.
    Todo,
}

impl Tag {
    /// All tags.
    const ALL: [Tag; 1] = [Tag::Todo];

    /// Its name, as stored and shown; only letters and `-`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Tag::Todo => "todo",
        }
    }

    /// The tag named `name`, in any case.
    pub(crate) fn from_name(name: &str) -> Option<Tag> {
        let name = name.to_lowercase();
        Tag::ALL.into_iter().find(|tag| tag.name() == name)
    }
}

/// A note's title, text and tag, with the tag taken out.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Tagged {
    /// Trimmed; `None` if empty.
    pub(crate) title: Option<String>,
    /// Trimmed; may be empty.
    pub(crate) text: String,
    pub(crate) tag: Option<Tag>,
}

/// Takes the tag out of a title, or out of the text if the title is empty.
pub(crate) fn parse(title: &str, text: &str) -> Tagged {
    let (title, text) = (title.trim(), text.trim());
    let (tag, title, text) = if !title.is_empty() {
        match split(title) {
            Some((tag, title)) => (Some(tag), title, text),
            None => (None, title, text),
        }
    } else {
        match split(text) {
            Some((tag, text)) => (Some(tag), title, text),
            None => (None, title, text),
        }
    };
    Tagged {
        title: Some(title.to_owned()).filter(|t| !t.is_empty()),
        text: text.to_owned(),
        tag,
    }
}

/// The tag `text` starts with, and the rest, trimmed. The tag is a word of
/// letters and `-`, then the end, whitespace, or `:` (which goes with it):
/// "todo: x" and "TODO x" have one, "todos", "todo-x" and "todo2" don't.
pub(crate) fn split(text: &str) -> Option<(Tag, &str)> {
    let end = (text.find(|c: char| !c.is_alphabetic() && c != '-')).unwrap_or(text.len());
    let tag = Tag::from_name(&text[..end])?;
    let rest = &text[end..];
    if !(rest.is_empty() || rest.starts_with(':') || rest.starts_with(char::is_whitespace)) {
        return None;
    }
    let rest = rest.strip_prefix(':').unwrap_or(rest);
    Some((tag, rest.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_is_a_word() {
        for (text, rest) in [
            ("TODO: x", "x"),
            ("todo x", "x"),
            ("todo", ""),
            ("ToDo:", ""),
            ("todo:x", "x"),
            ("todo \n x\n", "x"),
        ] {
            assert_eq!(split(text), Some((Tag::Todo, rest)), "{text:?}");
        }
        for text in [
            "todos", "todoist", "todo2", "todoé", "todo-x", "todo_x", "todo.", "tod", "", "a todo",
            " todo", "tödo",
        ] {
            assert_eq!(split(text), None, "{text:?}");
        }
    }

    fn tagged(title: Option<&str>, text: &str, tag: Option<Tag>) -> Tagged {
        Tagged {
            title: title.map(str::to_owned),
            text: text.to_owned(),
            tag,
        }
    }

    #[test]
    fn the_text_is_parsed_only_without_a_title() {
        let todo = Some(Tag::Todo);
        assert_eq!(parse("", " todo: x "), tagged(None, "x", todo));
        assert_eq!(parse(" ", "todo x"), tagged(None, "x", todo));
        assert_eq!(
            parse("Todo  Y", "todo x"),
            tagged(Some("Y"), "todo x", todo)
        );
        assert_eq!(parse("Y", "todo x"), tagged(Some("Y"), "todo x", None));
        // A title that's only the tag leaves the note unnamed.
        assert_eq!(parse("todo", "todo x"), tagged(None, "todo x", todo));
        assert_eq!(parse("", "plain"), tagged(None, "plain", None));
    }
}
