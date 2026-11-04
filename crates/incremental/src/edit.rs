//! Edit normalization (issue 030): a splice on the source, canonicalized
//! before the engine classifies damage around it.

/// One replacement: the bytes in `[start, end)` are replaced by `text`
/// (`start == end` inserts, `text` empty deletes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// First replaced byte.
    pub start: usize,
    /// One past the last replaced byte.
    pub end: usize,
    /// The replacement text.
    pub text: String,
}

impl Edit {
    /// A pure insertion before `start`.
    #[must_use]
    pub fn insert(start: usize, text: impl Into<String>) -> Self {
        Self {
            start,
            end: start,
            text: text.into(),
        }
    }

    /// A deletion of `[start, end)`.
    #[must_use]
    pub fn delete(start: usize, end: usize) -> Self {
        Self {
            start,
            end,
            text: String::new(),
        }
    }

    /// A replacement of `[start, end)`.
    #[must_use]
    pub fn replace(start: usize, end: usize, text: impl Into<String>) -> Self {
        Self {
            start,
            end,
            text: text.into(),
        }
    }

    /// Canonical form: ordered bounds, clipped to `source_len`. A
    /// backwards range is swapped; out-of-range ends clamp; the start
    /// saturates at the end.
    #[must_use]
    pub fn normalize(mut self, source_len: usize) -> Self {
        if self.start > self.end {
            std::mem::swap(&mut self.start, &mut self.end);
        }
        self.start = self.start.min(source_len);
        self.end = self.end.min(source_len);
        if self.end < self.start {
            self.end = self.start;
        }
        self
    }

    /// The splice applied to `source`.
    #[must_use]
    pub fn apply(&self, source: &str) -> String {
        let start = self.start.min(source.len());
        let end = self.end.min(source.len()).max(start);
        let mut out = String::with_capacity(source.len() + self.text.len());
        out.push_str(&source[..start]);
        out.push_str(&self.text);
        out.push_str(&source[end..]);
        out
    }

    /// Byte delta the edit imposes on positions past `end`.
    #[must_use]
    pub fn delta(&self) -> i64 {
        let removed = self.end.saturating_sub(self.start);
        i64::try_from(self.text.len()).unwrap_or(i64::MAX)
            - i64::try_from(removed).unwrap_or(i64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::Edit;

    #[test]
    fn normalize_orders_and_clips() {
        // Backwards ranges swap first, then clip to the source length.
        let edit = Edit::replace(7, 3, "xy").normalize(5);
        assert_eq!((edit.start, edit.end), (3, 5));
        let edit = Edit::replace(2, 9, "xy").normalize(5);
        assert_eq!((edit.start, edit.end), (2, 5));
    }

    #[test]
    fn apply_splices() {
        assert_eq!(Edit::insert(2, "XX").apply("abcd"), "abXXcd");
        assert_eq!(Edit::delete(1, 3).apply("abcd"), "ad");
        assert_eq!(Edit::replace(1, 3, "X").apply("abcd"), "aXd");
        assert_eq!(Edit::insert(0, "z").apply(""), "z");
    }

    #[test]
    fn delta_tracks_size_change() {
        assert_eq!(Edit::insert(1, "abc").delta(), 3);
        assert_eq!(Edit::delete(1, 4).delta(), -3);
        assert_eq!(Edit::replace(1, 3, "ab").delta(), 0);
    }
}
