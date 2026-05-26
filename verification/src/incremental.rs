//! Incremental reparse with the three-part predicate of Definition 3.2.
//!
//! `reuse_predicate` is annotated with Creusot contracts (`#[requires]` /
//! `#[ensures]`) that capture the three conditions of Definition 3.2 as a
//! pure-logic specification. Under `cargo build` / `cargo test` / `cargo kani`
//! the macros expand to no-ops; under `cargo creusot` Creusot translates the
//! function and its specification to Why3 and discharges the resulting VCs
//! via Z3 (Tier A: unbounded verification — supersedes the Kani-bounded
//! verification at §6.2).

#[allow(unused_imports)]
use creusot_std::{
    logic::{Int, Seq},
    prelude::{ensures, logic, pearlite, requires, trusted},
};

use crate::lexer::{next_token_lbp, ByteIndex};
#[cfg(creusot)]
use crate::lexer::next_token_lbp_logic;
use crate::parser::{BindingPower, Node, ParseTree};

#[derive(Debug, Clone, Copy)]
pub struct Edit {
    pub start: ByteIndex,
    pub added: usize,
    pub removed: usize,
}

// ---- Logic-level specification of Definition 3.2 -----------------------

/// Logic-level translate_to_new: returns the new-source position
/// corresponding to `old_pos` in the old source, or None if `old_pos`
/// lies inside the edited region. Mirrors `Edit::translate_to_new`.
#[logic(open)]
pub fn translate_logic(edit: Edit, old_pos: Int) -> Option<Int> {
    pearlite! {
        if old_pos < edit.start@ {
            Some(old_pos)
        } else if old_pos >= edit.start@ + edit.removed@ {
            Some(old_pos + edit.added@ - edit.removed@)
        } else {
            None
        }
    }
}

/// Byte at position `pos` of `src`; `None` if out of bounds.
/// Matches the runtime semantics of `src.get(pos).copied()`.
#[logic(open)]
pub fn byte_at(src: Seq<u8>, pos: Int) -> Option<u8> {
    pearlite! {
        if 0 <= pos && pos < src.len() {
            Some(src[pos])
        } else {
            None
        }
    }
}

/// Byte at an Optional position; chains `None` through.
/// Matches `pos.and_then(|p| src.get(p).copied())`.
#[logic(open)]
pub fn byte_at_opt(src: Seq<u8>, pos: Option<Int>) -> Option<u8> {
    pearlite! {
        match pos {
            Some(p) => byte_at(src, p),
            None => None,
        }
    }
}

/// Condition (1) of Definition 3.2: two-sided precedence band.
#[logic(open)]
pub fn cond_precedence_band(t_old: Node, m_new: BindingPower) -> bool {
    pearlite! { t_old.stop_lbp@ <= m_new@ && m_new@ < t_old.m_spine@ }
}

/// Condition (2) of Definition 3.2: text-region disjointness.
/// `T_old`'s old span does not overlap the edit's old range.
#[logic(open)]
pub fn cond_disjoint_region(t_old: Node, edit: Edit) -> bool {
    pearlite! {
        t_old.end@ <= edit.start@ || t_old.start@ >= edit.start@ + edit.removed@
    }
}

/// Condition (3) of Definition 3.2: tokenization-boundary agreement.
/// Bytes immediately before and after `T_old`'s span are unchanged across the edit.
#[logic(open)]
pub fn cond_boundaries_match(
    t_old: Node,
    edit: Edit,
    old_src: Seq<u8>,
    new_src: Seq<u8>,
) -> bool {
    pearlite! {
        // Before boundary: if t_old.start == 0 there's no byte to compare;
        // otherwise the byte at t_old.start - 1 in old_src must equal the
        // byte at translate(t_old.start - 1) in new_src.
        (t_old.start@ == 0
            || byte_at(old_src, t_old.start@ - 1)
                == byte_at_opt(new_src, translate_logic(edit, t_old.start@ - 1)))
        // After boundary: byte at t_old.end in old_src must equal the
        // byte at translate(t_old.end) in new_src.
        && byte_at(old_src, t_old.end@)
            == byte_at_opt(new_src, translate_logic(edit, t_old.end@))
    }
}

/// Condition (4) of Definition 3.2: next-token left-binding-power stability.
/// The lbp of the first non-whitespace token at or after the translated
/// position of `T_old.end` in the new source must equal `T_old.stop_lbp`.
/// Required because the lexer skips whitespace: an edit several bytes past
/// `T_old.end` can change the next non-whitespace token's identity even
/// when the immediate boundary byte (condition 3) is unchanged.
///
/// The match-on-translate is inlined as a three-way if/else to give the
/// SMT solver explicit case structure that mirrors `translate_logic`'s
/// definition; this avoids opaque case-analysis on `Option` patterns.
#[logic(open)]
pub fn cond_next_token_lbp(t_old: Node, edit: Edit, new_src: Seq<u8>) -> bool {
    pearlite! {
        if t_old.end@ < edit.start@ {
            next_token_lbp_logic(new_src, t_old.end@) == t_old.stop_lbp@
        } else if t_old.end@ >= edit.start@ + edit.removed@ {
            next_token_lbp_logic(new_src, t_old.end@ + edit.added@ - edit.removed@)
                == t_old.stop_lbp@
        } else {
            false
        }
    }
}

// ---- Operational code with Creusot contracts ---------------------------

impl Edit {
    #[ensures(result.0 == self.start && result.1@ == self.start@ + self.removed@)]
    #[requires(self.start@ + self.removed@ <= usize::MAX@)]
    pub fn old_range(&self) -> (ByteIndex, ByteIndex) {
        (self.start, self.start + self.removed)
    }

    /// Translate an old-source byte position to its new-source position,
    /// or `None` if `old_pos` is inside the edited region.
    #[requires(self.start@ + self.removed@ <= usize::MAX@)]
    #[requires(old_pos@ + self.added@ <= usize::MAX@)]
    #[ensures(match result {
        Some(p) => translate_logic(*self, old_pos@) == Some(p@),
        None => translate_logic(*self, old_pos@) == None,
    })]
    pub fn translate_to_new(&self, old_pos: ByteIndex) -> Option<ByteIndex> {
        if old_pos < self.start {
            Some(old_pos)
        } else if old_pos >= self.start + self.removed {
            Some(old_pos + self.added - self.removed)
        } else {
            None
        }
    }
}

/// Definition 3.2: the three-part reuse predicate.
///
/// Returns `true` iff all three structural conditions hold:
///   (1) `t_old.stop_lbp <= m_new < t_old.m_spine`
///   (2) `t_old`'s old span is disjoint from the edit's old range
///   (3) bytes immediately adjacent to `t_old`'s span are unchanged
///
/// **Creusot specification:** `result == cond_precedence_band && cond_disjoint_region && cond_boundaries_match`.
/// This is the unbounded counterpart of the Kani harnesses 6.4d / 6.4f
/// (§6.2 of Paper 2); where those harnesses checked the bi-implication for
/// symbolic inputs up to 32 bytes, the Creusot contract holds for `Seq<u8>`s
/// of any length.
#[requires(edit.start@ + edit.removed@ <= usize::MAX@)]
#[requires(t_old.end@ + edit.added@ <= usize::MAX@)]
#[requires(t_old.start@ <= t_old.end@)]
#[requires(t_old.end@ + edit.added@ - edit.removed@ <= new_src@.len())]
#[requires(edit.start@ + edit.added@ <= new_src@.len())]
#[ensures(result == (
    cond_precedence_band(*t_old, m_new)
    && cond_disjoint_region(*t_old, *edit)
    && cond_boundaries_match(*t_old, *edit, old_src@, new_src@)
    && cond_next_token_lbp(*t_old, *edit, new_src@)
))]
pub fn reuse_predicate(
    t_old: &Node,
    m_new: BindingPower,
    edit: &Edit,
    old_src: &[u8],
    new_src: &[u8],
) -> bool {
    // (1) Two-sided precedence band
    if !(t_old.stop_lbp <= m_new && m_new < t_old.m_spine) {
        return false;
    }

    // (2) Text-region disjointness
    let (edit_start, edit_end) = edit.old_range();
    if t_old.end > edit_start && t_old.start < edit_end {
        return false;
    }

    // (3) Tokenization-boundary agreement: bytes adjacent to T_old.span unchanged
    if t_old.start > 0 {
        let old_byte_before = old_src.get(t_old.start - 1).copied();
        let new_pos_before = edit.translate_to_new(t_old.start - 1);
        let new_byte_before = new_pos_before.and_then(|p| new_src.get(p).copied());
        if old_byte_before != new_byte_before {
            return false;
        }
    }
    let old_byte_after = old_src.get(t_old.end).copied();
    let new_pos_after = edit.translate_to_new(t_old.end);
    let new_byte_after = new_pos_after.and_then(|p| new_src.get(p).copied());
    if old_byte_after != new_byte_after {
        return false;
    }

    // (4) Next-token lbp stability: the lbp of the next non-whitespace
    // token at translate(T_old.end) in new_src must equal T_old.stop_lbp.
    // Required because the lexer skips whitespace — an edit several bytes
    // past T_old.end can change the next non-whitespace token even when
    // condition (3)'s boundary byte is unchanged.
    let new_end = match new_pos_after {
        Some(p) => p,
        None => return false,
    };
    if next_token_lbp(new_src, new_end) != t_old.stop_lbp {
        return false;
    }

    true
}

/// Top-level incremental reparse.
///
/// PHASE 3 PLACEHOLDER: currently re-parses from scratch.
#[trusted]
pub fn reparse(_t_old: &ParseTree, _edit: &Edit, new_src: &[u8]) -> Option<ParseTree> {
    crate::parser::batch_parse(new_src)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::batch_parse;

    #[test]
    fn predicate_basic() {
        let src = b"1+2";
        let t = batch_parse(src).unwrap();
        let edit = Edit { start: 0, added: 0, removed: 0 };
        assert!(reuse_predicate(t.root_node(), 0, &edit, src, src));
    }

    #[test]
    fn predicate_overlapping_edit() {
        let src = b"1+2";
        let t = batch_parse(src).unwrap();
        let edit = Edit { start: 0, added: 1, removed: 1 };
        let new_src = b"3+2";
        assert!(!reuse_predicate(t.root_node(), 0, &edit, src, new_src));
    }
}
