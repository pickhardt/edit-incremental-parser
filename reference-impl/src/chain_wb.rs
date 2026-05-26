//! Weight-balanced concatenation for associativity-conflict chains
//! (`chain_splice` feature). The general splice (`incremental.rs::
//! try_splice_general`) decomposes an edited chain into O(log n) off-path
//! subtree *units* plus a reparsed middle, then must reassemble them into one
//! balanced chain. Building a fresh spine *over* the units nests a heavy unit's
//! spine under the new one, so depth creeps up across many edits. Instead we
//! `join` the units with a weight-balanced concatenation that *merges* their
//! spines (with rotations), so the result is a weight-balanced tree of all the
//! operands and depth stays O(log n) across unbounded edits.
//!
//! This is a leaf-based weight-balanced (BB[α]) sequence tree: operands are the
//! leaves, associativity-conflict `Binary` nodes are the spine. `wb` (Node) is
//! the subtree operand count, read in O(1) via `chain_size`.
//!
//! Byte spans: `join` only ever combines *source-adjacent* pieces and rotations
//! preserve in-order, so a combined node spans `[left.lo, right.hi)` with all
//! the bytes (operators + whitespace) in between — exactly the source range. A
//! child's span is recovered from its parent's span and the child's stored
//! `width` (`children`), so we never need to store absolute positions.

use std::sync::Arc;

use crate::ast::{Node, NodeKind};
use crate::lexer::TokenKind;
use crate::parser::{chain_size, ChainOperand};

/// Weight-balance ratio: a node's heavier child may be at most `DELTA`× the
/// lighter. 3 is the standard safe integer choice for BB[α] leaf trees.
const DELTA: u32 = 3;

/// A chain subtree together with its absolute byte span `[lo, hi)` in the new
/// source. `size` (operand count) is read on demand via `chain_size`.
struct Piece {
    node: Arc<Node>,
    lo: u32,
    hi: u32,
}

#[inline]
fn size(p: &Piece, op: TokenKind) -> u32 {
    chain_size(&p.node, op)
}

/// Decompose a spine node into its two children as pieces. Child spans are
/// recovered from the parent span and the children's stored widths (the
/// operator/whitespace gap lands between them, inside the parent span).
/// Precondition: `p.node` is `Binary{op}` (only called on spine nodes).
fn children(p: &Piece, op: TokenKind) -> (Piece, Piece) {
    match &p.node.kind {
        NodeKind::Binary { op: o, left, right } if *o == op => {
            let lhs = Piece { node: Arc::clone(left), lo: p.lo, hi: p.lo + left.width };
            let rhs = Piece { node: Arc::clone(right), lo: p.hi - right.width, hi: p.hi };
            (lhs, rhs)
        }
        _ => unreachable!("children() called on a non-spine piece"),
    }
}

/// Combine two source-adjacent pieces into one spine node (no rebalancing).
fn cnode(op: TokenKind, l: Piece, r: Piece, lbp: u32) -> Piece {
    let wb = size(&l, op) + size(&r, op);
    let (lo, hi) = (l.lo, r.hi);
    let node = Arc::new(Node {
        kind: NodeKind::Binary { op, left: l.node, right: r.node },
        width: hi - lo,
        // Interior spine nodes get an empty acceptance band
        // (stop_lbp == m_spine == chain_lbp) so they are never standalone-
        // reused; the chain root's stop_lbp is restamped by the caller's
        // parse_expr loop. Mirrors `build_balanced`.
        m_spine: lbp,
        stop_lbp: lbp,
        m_floor: lbp.saturating_sub(1),
        id: crate::ast::fresh_id(),
        wb,
    });
    Piece { node, lo, hi }
}

#[inline]
fn balanced(a: u32, b: u32) -> bool {
    a <= DELTA * b && b <= DELTA * a
}

/// Weight-balanced concatenation of two balanced chain pieces (`l` left of `r`
/// in source). Returns a balanced piece containing all of `l`'s then `r`'s
/// operands. O(|log(size l) − log(size r)|).
fn join(op: TokenKind, l: Piece, r: Piece, lbp: u32) -> Piece {
    let sl = size(&l, op);
    let sr = size(&r, op);
    if balanced(sl, sr) {
        return cnode(op, l, r, lbp);
    }
    if sl > sr {
        // `l` is too heavy (sl > DELTA·sr ⇒ sl ≥ 4 ⇒ `l` is a spine node).
        // Descend its right edge: join `r` onto `l`'s right child, rebalance.
        let (a, b) = children(&l, op);
        let nb = join(op, b, r, lbp);
        let sa = size(&a, op);
        let snb = size(&nb, op);
        if balanced(sa, snb) {
            cnode(op, a, nb, lbp)
        } else {
            // nb too heavy relative to a; rotate.
            let (c, d) = children(&nb, op);
            if size(&c, op) <= size(&d, op) {
                // single rotation: (a · (c · d)) ↦ ((a · c) · d)
                let ac = cnode(op, a, c, lbp);
                cnode(op, ac, d, lbp)
            } else {
                // double rotation: c = (c1 · c2) ↦ ((a · c1) · (c2 · d))
                let (c1, c2) = children(&c, op);
                let ac1 = cnode(op, a, c1, lbp);
                let c2d = cnode(op, c2, d, lbp);
                cnode(op, ac1, c2d, lbp)
            }
        }
    } else {
        // `r` too heavy; symmetric, descend its left edge.
        let (a, b) = children(&r, op);
        let na = join(op, l, a, lbp);
        let sb = size(&b, op);
        let sna = size(&na, op);
        if balanced(sna, sb) {
            cnode(op, na, b, lbp)
        } else {
            let (c, d) = children(&na, op);
            if size(&d, op) <= size(&c, op) {
                // single rotation: ((c · d) · b) ↦ (c · (d · b))
                let db = cnode(op, d, b, lbp);
                cnode(op, c, db, lbp)
            } else {
                // double rotation: d = (d1 · d2) ↦ ((c · d1) · (d2 · b))
                let (d1, d2) = children(&d, op);
                let cd1 = cnode(op, c, d1, lbp);
                let d2b = cnode(op, d2, b, lbp);
                cnode(op, cd1, d2b, lbp)
            }
        }
    }
}

/// Concatenate `units` (in source order, ≥ 1) into one weight-balanced chain
/// tree. Each unit carries its new-source `start_byte`; its span is
/// `[start, start + width)`. Folds left via the weight-balanced `join`, which
/// keeps every intermediate result balanced, so the final tree is balanced and
/// O(log n) deep regardless of the units' relative sizes.
pub(crate) fn join_all(op: TokenKind, units: &[ChainOperand], lbp: u32) -> Arc<Node> {
    let piece_of = |u: &ChainOperand| Piece {
        node: Arc::clone(&u.node),
        lo: u.start_byte,
        hi: u.start_byte + u.node.width,
    };
    let mut acc = piece_of(&units[0]);
    for u in &units[1..] {
        acc = join(op, acc, piece_of(u), lbp);
    }
    acc.node
}
