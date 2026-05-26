//! Recovery cost algebra for the calculator-grammar instance (Paper §4 / M6).
//!
//! Kani verifies the LOCAL component that Theorem 4.2's proof actually leans
//! on: the min-plus tropical cost monoid (`add` / `min` over non-negative,
//! overflow-safe repair costs). The corrected Theorem 4.2 verdict
//! (proof_sketches.txt) states that this monoid — additivity of non-negative
//! costs over disjoint regions — is what carries the proof; the harnesses in
//! `harnesses.rs` discharge exactly its laws.
//!
//! NOT verified here, and stated as such in the paper: the cost-optimal repair
//! *search* and the sole-error/skeleton decision. Both are parser-dependent
//! and on the wrong side of Kani's parser-symbolic-execution frontier (§6.3,
//! the same wall that keeps the parser itself out of Kani). Those rest on the
//! paper proof plus the 2000-case property test
//! (`reference-impl/tests/recovery_theorem.rs`).

/// Repair cost, valued in the min-plus tropical semiring `(Cost ∪ {∞}, min, +)`.
pub type Cost = u32;

/// Absorbing element for `+` / identity for `min`. `u32::MAX / 2` so two
/// in-range costs never overflow `u32` before the cap is applied.
pub const INFINITY: Cost = u32::MAX / 2;

/// Additive combine of the semiring (`+`), saturating at `INFINITY` so cost
/// accumulation is overflow-safe. This is the monoid the recovery-composition
/// theorem uses to add repair costs across disjoint regions:
/// `cost(global) = add(cost_inside, cost_outside)`.
#[inline]
pub fn add(a: Cost, b: Cost) -> Cost {
    let s = a.saturating_add(b);
    if s > INFINITY {
        INFINITY
    } else {
        s
    }
}

/// The `min` of the semiring: choose the cheaper of two complete repairs.
#[inline]
pub fn min(a: Cost, b: Cost) -> Cost {
    if a <= b {
        a
    } else {
        b
    }
}

/// A token-stream-level repair. Mirrors `reference-impl`'s `Repair` for the cost
/// model; the token payloads are irrelevant to the algebra verified here, so
/// they are elided.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(creusot), derive(PartialEq, Eq))]
pub enum Repair {
    Insert,
    Delete,
    Substitute,
}

impl Repair {
    /// Uniform unit cost (configurable in `reference-impl`; unit here). Strictly
    /// positive, so adding a repair strictly increases the accumulated cost.
    #[inline]
    pub fn cost(self) -> Cost {
        1
    }
}

/// Total min-plus cost of a repair sequence: the additive (monoid) fold.
#[inline]
pub fn total(repairs: &[Repair]) -> Cost {
    let mut acc: Cost = 0;
    let mut i = 0;
    while i < repairs.len() {
        acc = add(acc, repairs[i].cost());
        i += 1;
    }
    acc
}
