//! Edit representation. GENERATED-CRATE RUNTIME (grammar-independent).
//!
//! An `Edit` replaces the byte range `[start, end)` in the old source with
//! `replacement`. We derive a byte-mapping that translates an old offset to
//! its new position (or `None` if inside the edited region).

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

    /// Map an OLD byte offset to its position in NEW source. `None` for
    /// offsets inside `[start, end)` (the edited region).
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

    /// True iff OLD range `[old_start, old_end)` is entirely outside the edit.
    pub fn old_range_unchanged(&self, old_start: u32, old_end: u32) -> bool {
        old_end <= self.start || old_start >= self.end
    }

    /// Translate an unchanged OLD range to its NEW position; `None` if any
    /// byte was edited.
    pub fn translate_old_range(&self, old_start: u32, old_end: u32) -> Option<(u32, u32)> {
        if !self.old_range_unchanged(old_start, old_end) {
            return None;
        }
        let ns = self.map_old_to_new(old_start)?;
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
