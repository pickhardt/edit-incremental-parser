//! Machine-checked certificate of the reuse predicate (GENERATED-CRATE
//! RUNTIME). The four-part precedence-bounded reuse predicate, restated as
//! a standalone byte-level function over symbolic inputs, plus Kani
//! harnesses that verify — exhaustively over all bounded symbolic
//! (node, edit, old source, new source) tuples — that the operational
//! predicate agrees with the declarative specification of Definition 3.3,
//! and never panics or indexes out of bounds.
//!
//! The only grammar-specific dependency is `crate::op::next_token_lbp`
//! (condition 4), which the generator specialises to the grammar's
//! operator set. Running `cargo kani` re-discharges this per grammar:
//! the per-grammar certificate the PoC is about.

/// Node reuse metadata (the cached subtree's certificate inputs).
#[derive(Clone, Copy)]
pub struct VNode {
    pub start: usize,
    pub end: usize,
    pub m_spine: u32,
    pub stop_lbp: u32,
}

/// A byte-level edit: replace `[start, start+removed)` with `added` bytes.
#[derive(Clone, Copy)]
pub struct VEdit {
    pub start: usize,
    pub added: usize,
    pub removed: usize,
}

/// Old-source position -> new-source position, or `None` if inside the edit.
pub fn translate(edit: VEdit, old_pos: usize) -> Option<usize> {
    if old_pos < edit.start {
        Some(old_pos)
    } else if old_pos >= edit.start + edit.removed {
        Some(old_pos + edit.added - edit.removed)
    } else {
        None
    }
}

fn byte_at(src: &[u8], pos: usize) -> Option<u8> {
    src.get(pos).copied()
}
fn byte_at_opt(src: &[u8], pos: Option<usize>) -> Option<u8> {
    pos.and_then(|p| src.get(p).copied())
}

// ---- Declarative specification (Definition 3.3) ------------------------

pub fn spec_band(n: VNode, m_new: u32) -> bool {
    n.stop_lbp <= m_new && m_new < n.m_spine
}
pub fn spec_disjoint(n: VNode, e: VEdit) -> bool {
    n.end <= e.start || n.start >= e.start + e.removed
}
pub fn spec_boundaries(n: VNode, e: VEdit, old: &[u8], new: &[u8]) -> bool {
    let before = n.start == 0
        || byte_at(old, n.start - 1) == byte_at_opt(new, translate(e, n.start - 1));
    let after = byte_at(old, n.end) == byte_at_opt(new, translate(e, n.end));
    before && after
}
pub fn spec_next_token(n: VNode, e: VEdit, new: &[u8]) -> bool {
    match translate(e, n.end) {
        Some(p) => crate::op::next_token_lbp(new, p) == n.stop_lbp,
        None => false,
    }
}

// ---- Operational predicate (early-return style) ------------------------

/// The four-part predicate as the parser evaluates it. Returns `true` iff
/// the cached subtree `n` is reusable as the result of `parse_expr(m_new)`
/// after edit `e`.
pub fn reuse_predicate(n: VNode, m_new: u32, e: VEdit, old: &[u8], new: &[u8]) -> bool {
    // (1) precedence band
    if !(n.stop_lbp <= m_new && m_new < n.m_spine) {
        return false;
    }
    // (2) text-region disjointness
    if n.end > e.start && n.start < e.start + e.removed {
        return false;
    }
    // (3) tokenization-boundary agreement
    if n.start > 0 {
        let ob = byte_at(old, n.start - 1);
        let nb = byte_at_opt(new, translate(e, n.start - 1));
        if ob != nb {
            return false;
        }
    }
    if byte_at(old, n.end) != byte_at_opt(new, translate(e, n.end)) {
        return false;
    }
    // (4) next-token lbp stability
    let new_end = match translate(e, n.end) {
        Some(p) => p,
        None => return false,
    };
    if crate::op::next_token_lbp(new, new_end) != n.stop_lbp {
        return false;
    }
    true
}

// ---- Kani harnesses ----------------------------------------------------
//
// Bounded-exhaustive over all symbolic inputs up to BOUND source bytes per
// side. Kani additionally proves the absence of panics, integer overflow,
// and out-of-bounds indexing throughout the predicate.

#[cfg(kani)]
mod harness {
    use super::*;

    const BOUND: usize = 6;

    fn any_node() -> VNode {
        let start: usize = kani::any();
        let end: usize = kani::any();
        kani::assume(start <= end && end <= BOUND);
        VNode { start, end, m_spine: kani::any(), stop_lbp: kani::any() }
    }

    fn any_edit() -> VEdit {
        let start: usize = kani::any();
        let removed: usize = kani::any();
        let added: usize = kani::any();
        kani::assume(start <= BOUND && removed <= BOUND && added <= BOUND);
        kani::assume(start + removed <= BOUND);
        VEdit { start, added, removed }
    }

    /// Soundness + completeness of the operational predicate against the
    /// declarative specification of Definition 3.3, for every bounded
    /// symbolic (node, m_new, edit, old, new) tuple.
    #[kani::proof]
    #[kani::unwind(8)]
    fn predicate_matches_spec() {
        let n = any_node();
        let e = any_edit();
        let m_new: u32 = kani::any();
        let old: [u8; BOUND] = kani::any();
        let new: [u8; BOUND] = kani::any();

        let got = reuse_predicate(n, m_new, e, &old, &new);
        let want = spec_band(n, m_new)
            && spec_disjoint(n, e)
            && spec_boundaries(n, e, &old, &new)
            && spec_next_token(n, e, &new);
        assert_eq!(got, want);
    }

    /// The load-bearing direction on its own: a `true` result implies every
    /// structural condition holds (the parser only reuses when sound).
    #[kani::proof]
    #[kani::unwind(8)]
    fn reuse_implies_sound() {
        let n = any_node();
        let e = any_edit();
        let m_new: u32 = kani::any();
        let old: [u8; BOUND] = kani::any();
        let new: [u8; BOUND] = kani::any();

        if reuse_predicate(n, m_new, e, &old, &new) {
            assert!(spec_band(n, m_new));
            assert!(spec_disjoint(n, e));
            assert!(spec_boundaries(n, e, &old, &new));
            assert!(spec_next_token(n, e, &new));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_edit_reuses() {
        // No-op edit on "1+2"-like source: a node spanning [0,1] with a
        // wide band, boundary byte after it unchanged.
        let old = b"1+1";
        // Node "1" at [0,1) is followed by "+"; its stop_lbp is lbp("+"),
        // so it is reusable exactly at floors >= that lbp.
        let stop = crate::op::next_token_lbp(old, 1);
        let n = VNode { start: 0, end: 1, m_spine: u32::MAX, stop_lbp: stop };
        let e = VEdit { start: 3, added: 0, removed: 0 };
        assert!(reuse_predicate(n, stop, e, old, old));
    }

    #[test]
    fn overlapping_edit_rejected() {
        let old = b"1+1";
        let n = VNode { start: 0, end: 1, m_spine: u32::MAX, stop_lbp: crate::op::next_token_lbp(old, 1) };
        let e = VEdit { start: 0, added: 1, removed: 1 };
        let new = b"2+1";
        assert!(!reuse_predicate(n, 0, e, old, new));
    }
}
