//! [`History`]: previously entered command lines.

/// The command lines entered so far, browsable like in a shell: going back
/// from the line being typed, and forward again to it.
#[derive(Debug, Default)]
pub struct History {
    lines: Vec<String>,
    /// The line shown while browsing; `None` when not browsing.
    position: Option<usize>,
    /// The line being typed when browsing started.
    draft: String,
}

impl History {
    /// An empty history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an entered line, and stops browsing. Blank lines and repeats of
    /// the previous line are skipped.
    pub fn push(&mut self, line: &str) {
        self.position = None;
        if !line.trim().is_empty() && self.lines.last().is_none_or(|last| last != line) {
            self.lines.push(line.to_owned());
        }
    }

    /// Stops browsing, e.g. because the user edited the line. The next
    /// [`back`](History::back) starts from the newest line again.
    pub fn stop_browsing(&mut self) {
        self.position = None;
    }

    /// Goes back one line. `current` is the line being typed; it's kept, and
    /// [`forward`](History::forward) returns to it. Returns `None` if there's no
    /// older line.
    pub fn back(&mut self, current: &str) -> Option<&str> {
        let position = match self.position {
            None => {
                let newest = self.lines.len().checked_sub(1)?;
                self.draft = current.to_owned();
                newest
            }
            Some(0) => return None,
            Some(p) => p - 1,
        };
        self.position = Some(position);
        Some(&self.lines[position])
    }

    /// Goes forward one line, ending at the line that was being typed.
    /// Returns `None` if not browsing.
    pub fn forward(&mut self) -> Option<&str> {
        let position = self.position?;
        if position + 1 < self.lines.len() {
            self.position = Some(position + 1);
            Some(&self.lines[position + 1])
        } else {
            self.position = None;
            Some(&self.draft)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(lines: &[&str]) -> History {
        let mut h = History::new();
        lines.iter().for_each(|l| h.push(l));
        h
    }

    #[test]
    fn browses_back_and_forward_to_the_draft() {
        let mut h = history(&["a", "b"]);
        assert_eq!(h.back("dra"), Some("b"));
        assert_eq!(h.back("b"), Some("a"));
        assert_eq!(h.back("a"), None);
        assert_eq!(h.forward(), Some("b"));
        assert_eq!(h.forward(), Some("dra"));
        assert_eq!(h.forward(), None);
    }

    #[test]
    fn empty_history_has_nothing_to_browse() {
        let mut h = History::new();
        assert_eq!(h.back("x"), None);
        assert_eq!(h.forward(), None);
    }

    #[test]
    fn skips_blank_lines_and_repeats() {
        let mut h = history(&["a", " ", "a", "b", "a"]);
        assert_eq!(h.back(""), Some("a"));
        assert_eq!(h.back(""), Some("b"));
        assert_eq!(h.back(""), Some("a"));
        assert_eq!(h.back(""), None);
    }

    #[test]
    fn pushing_or_editing_stops_browsing() {
        let mut h = history(&["a", "b"]);
        h.back("");
        h.back("");
        h.stop_browsing();
        assert_eq!(h.back("edited"), Some("b"));
        h.push("c");
        assert_eq!(h.forward(), None);
        assert_eq!(h.back(""), Some("c"));
    }
}
