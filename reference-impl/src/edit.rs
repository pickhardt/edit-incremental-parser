//! Edit representation.
//!
//! An `Edit` replaces the byte range `[start, end)` in the old source with
//! `replacement`. Applying it produces the new source. We also derive a
//! byte-mapping function that translates an old byte offset to its new
//! position (or `None` if it's inside the edited region).

#[derive(Debug, Clone)]
pub struct Edit {
    pub start: u32,
    pub end: u32,
    pub replacement: String,
}

impl Edit {
    pub fn apply(&self, src: &str) -> String {
        let mut out = String::with_capacity(src.len() + self.replacement.len());
        out.push_str(&src[..self.start as usize]);
        out.push_str(&self.replacement);
        out.push_str(&src[self.end as usize..]);
        out
    }

    /// Map an OLD byte offset to its position in NEW source.
    /// Returns `None` for offsets inside `[start, end)` (edited region).
    pub fn map_old_to_new(&self, old_offset: u32) -> Option<u32> {
        if old_offset < self.start {
            Some(old_offset)
        } else if old_offset >= self.end {
            let delta = self.replacement.len() as i64 - (self.end - self.start) as i64;
            Some((old_offset as i64 + delta) as u32)
        } else {
            None
        }
    }

    /// True iff the OLD byte range `[old_start, old_end)` is entirely
    /// outside the edit's range (i.e., the bytes are textually
    /// unchanged in the new source).
    pub fn old_range_unchanged(&self, old_start: u32, old_end: u32) -> bool {
        old_end <= self.start || old_start >= self.end
    }

    /// Translate an OLD range that's entirely unchanged to its NEW
    /// position. Returns `None` if any byte in the range was edited.
    pub fn translate_old_range(&self, old_start: u32, old_end: u32) -> Option<(u32, u32)> {
        if !self.old_range_unchanged(old_start, old_end) {
            return None;
        }
        let ns = self.map_old_to_new(old_start)?;
        // For old_end (exclusive), translate via end-1 if non-empty,
        // then add 1; or use end directly if it's past the edit.
        let ne = if old_end == old_start {
            ns
        } else if old_end <= self.start {
            old_end
        } else {
            let delta = self.replacement.len() as i64 - (self.end - self.start) as i64;
            (old_end as i64 + delta) as u32
        };
        Some((ns, ne))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_basic() {
        let e = Edit {
            start: 2,
            end: 3,
            replacement: "XYZ".to_string(),
        };
        assert_eq!(e.apply("abcdef"), "abXYZdef");
    }

    #[test]
    fn unchanged_before() {
        let e = Edit {
            start: 5,
            end: 8,
            replacement: "x".to_string(),
        };
        assert!(e.old_range_unchanged(0, 5));
        assert_eq!(e.translate_old_range(0, 5), Some((0, 5)));
    }

    #[test]
    fn unchanged_after() {
        let e = Edit {
            start: 5,
            end: 8,
            replacement: "x".to_string(),
        };
        // Old `[8, 12)` → after edit, delta = 1 - 3 = -2 → `[6, 10)`.
        assert_eq!(e.translate_old_range(8, 12), Some((6, 10)));
    }

    #[test]
    fn changed_overlap() {
        let e = Edit {
            start: 5,
            end: 8,
            replacement: "x".to_string(),
        };
        assert!(!e.old_range_unchanged(4, 6));
        assert_eq!(e.translate_old_range(4, 6), None);
    }
}
