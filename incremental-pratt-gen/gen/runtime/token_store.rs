//! Persistent, spliceable token store (GENERATED-CRATE RUNTIME,
//! grammar-independent).
//!
//! An implicit treap of tokens stored with **relative offsets** — each
//! entry carries its `gap` (bytes skipped before it: whitespace/unknown)
//! and `width`, not an absolute position. Absolute positions are derived
//! by a prefix sum during traversal, so splicing tokens in the middle
//! leaves the entire suffix untouched (no position rewrite). This is the
//! red/green-tree trick (used for AST nodes) applied to the token stream.
//!
//! Operations:
//!   * `from_tokens` — build from a full tokenization (O(n)).
//!   * `index_at_byte` — first token whose span ends past a byte. O(log n).
//!   * `splice` — replace a token-index range with new specs. O(log n + k).
//!   * `get` / `to_vec` — read tokens back with absolute positions.
//!
//! Purpose: a persistent full token stream that survives edits, so
//! incremental relexing (relex-to-resynchronization) can update it in
//! O(edit + resync) rather than re-lexing the file — serving syntax
//! highlighting, semantic tokens, and other whole-stream consumers. The
//! parser itself uses demand-driven lexing (see `cursor.rs`) and does not
//! read from this store; the bench is unaffected by design.

use crate::lexer::{Token, TokenKind};

/// A token as stored: kind + relative byte offsets. `gap` is the number of
/// bytes (whitespace or unknown) skipped before this token since the end of
/// the previous one; `width` is the token's own byte length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenSpec {
    pub kind: TokenKind,
    pub gap: u32,
    pub width: u32,
}

impl TokenSpec {
    fn span(&self) -> u64 {
        self.gap as u64 + self.width as u64
    }
}

struct Node {
    spec: TokenSpec,
    prio: u64,
    size: u32, // token count in subtree
    bytes: u64, // total byte span (gap+width) in subtree
    l: Tree,
    r: Tree,
}
type Tree = Option<Box<Node>>;

fn size(t: &Tree) -> u32 {
    t.as_ref().map_or(0, |n| n.size)
}
fn bytes(t: &Tree) -> u64 {
    t.as_ref().map_or(0, |n| n.bytes)
}
fn update(n: &mut Node) {
    n.size = 1 + size(&n.l) + size(&n.r);
    n.bytes = n.spec.span() + bytes(&n.l) + bytes(&n.r);
}

fn merge(a: Tree, b: Tree) -> Tree {
    match (a, b) {
        (None, b) => b,
        (a, None) => a,
        (Some(mut na), Some(mut nb)) => {
            if na.prio >= nb.prio {
                na.r = merge(na.r.take(), Some(nb));
                update(&mut na);
                Some(na)
            } else {
                nb.l = merge(Some(na), nb.l.take());
                update(&mut nb);
                Some(nb)
            }
        }
    }
}

/// Split into (first `k` tokens, the rest).
fn split(t: Tree, k: u32) -> (Tree, Tree) {
    match t {
        None => (None, None),
        Some(mut n) => {
            let ls = size(&n.l);
            if k <= ls {
                let (a, b) = split(n.l.take(), k);
                n.l = b;
                update(&mut n);
                (a, Some(n))
            } else {
                let (a, b) = split(n.r.take(), k - ls - 1);
                n.r = a;
                update(&mut n);
                (Some(n), b)
            }
        }
    }
}

/// First token index whose cumulative byte span exceeds `byte` (i.e. the
/// token containing `byte`, or the next token if `byte` falls in a gap).
/// Returns the token count if `byte` is at/after the end.
fn idx_at_byte(t: &Tree, base: u64, byte: u64) -> u32 {
    match t {
        None => 0,
        Some(n) => {
            let lb = bytes(&n.l);
            let through = base + lb + n.spec.span();
            if byte < base + lb {
                idx_at_byte(&n.l, base, byte)
            } else if byte < through {
                size(&n.l)
            } else {
                size(&n.l) + 1 + idx_at_byte(&n.r, through, byte)
            }
        }
    }
}

pub struct TokenStore {
    root: Tree,
    rng: u64,
}

impl TokenStore {
    pub fn new() -> Self {
        TokenStore { root: None, rng: 0x2545F4914F6CDD1D }
    }

    fn next_prio(&mut self) -> u64 {
        // xorshift64* — deterministic, good enough for treap balance.
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    fn node(&mut self, spec: TokenSpec) -> Tree {
        let prio = self.next_prio();
        let mut n = Box::new(Node { spec, prio, size: 0, bytes: 0, l: None, r: None });
        update(&mut n);
        Some(n)
    }

    fn tree_from_specs(&mut self, specs: &[TokenSpec]) -> Tree {
        let mut t: Tree = None;
        for s in specs {
            let leaf = self.node(*s);
            t = merge(t, leaf);
        }
        t
    }

    /// Build from a full tokenization (absolute positions -> relative specs).
    pub fn from_tokens(tokens: &[Token]) -> Self {
        let mut store = TokenStore::new();
        let mut prev_end = 0u32;
        let specs: Vec<TokenSpec> = tokens
            .iter()
            .map(|t| {
                let s = TokenSpec { kind: t.kind, gap: t.start - prev_end, width: t.end - t.start };
                prev_end = t.end;
                s
            })
            .collect();
        store.root = store.tree_from_specs(&specs);
        store
    }

    pub fn len(&self) -> usize {
        size(&self.root) as usize
    }
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }
    pub fn byte_len(&self) -> u64 {
        bytes(&self.root)
    }

    /// First token index whose span ends past `byte`. O(log n).
    pub fn index_at_byte(&self, byte: u64) -> usize {
        idx_at_byte(&self.root, 0, byte) as usize
    }

    /// Replace tokens `[lo, hi)` with `new`. O(log n + new.len()).
    pub fn splice(&mut self, lo: usize, hi: usize, new: &[TokenSpec]) {
        let root = self.root.take();
        let (ab, c) = split(root, hi as u32);
        let (a, _mid) = split(ab, lo as u32);
        let mid = self.tree_from_specs(new);
        self.root = merge(merge(a, mid), c);
    }

    /// Token at index `i` with absolute positions. O(log n).
    pub fn get(&self, i: usize) -> Option<Token> {
        let mut node = self.root.as_deref();
        let mut idx = i as u32;
        let mut base = 0u64; // bytes before the current subtree
        while let Some(n) = node {
            let ls = size(&n.l);
            if idx < ls {
                node = n.l.as_deref();
            } else if idx == ls {
                let start = base + bytes(&n.l) + n.spec.gap as u64;
                return Some(Token {
                    kind: n.spec.kind,
                    start: start as u32,
                    end: (start + n.spec.width as u64) as u32,
                });
            } else {
                base += bytes(&n.l) + n.spec.span();
                idx -= ls + 1;
                node = n.r.as_deref();
            }
        }
        None
    }

    /// Stored spec (kind + relative gap/width) at index `i`. O(log n).
    pub fn spec_at(&self, i: usize) -> Option<TokenSpec> {
        let mut node = self.root.as_deref();
        let mut idx = i as u32;
        while let Some(n) = node {
            let ls = size(&n.l);
            if idx < ls {
                node = n.l.as_deref();
            } else if idx == ls {
                return Some(n.spec);
            } else {
                idx -= ls + 1;
                node = n.r.as_deref();
            }
        }
        None
    }

    /// All tokens with absolute positions (for tests / whole-stream consumers).
    pub fn to_vec(&self) -> Vec<Token> {
        let mut out = Vec::with_capacity(self.len());
        let mut offset = 0u64;
        collect(&self.root, &mut offset, &mut out);
        out
    }
}

impl Default for TokenStore {
    fn default() -> Self {
        TokenStore::new()
    }
}

fn collect(t: &Tree, offset: &mut u64, out: &mut Vec<Token>) {
    if let Some(n) = t {
        collect(&n.l, offset, out);
        let start = *offset + n.spec.gap as u64;
        let end = start + n.spec.width as u64;
        out.push(Token { kind: n.spec.kind, start: start as u32, end: end as u32 });
        *offset = end;
        collect(&n.r, offset, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // Reference model: a flat Vec of specs with the same operations.
    fn ref_to_tokens(specs: &[TokenSpec]) -> Vec<Token> {
        let mut out = Vec::new();
        let mut off = 0u32;
        for s in specs {
            let start = off + s.gap;
            let end = start + s.width;
            out.push(Token { kind: s.kind, start, end });
            off = end;
        }
        out
    }

    fn arb_spec() -> impl Strategy<Value = TokenSpec> {
        (0u32..4, 1u32..6).prop_map(|(gap, width)| TokenSpec { kind: TokenKind::Eof, gap, width })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 4000, ..ProptestConfig::default() })]

        /// Build + a sequence of random splices must match the flat reference
        /// model token-for-token (positions included), and index_at_byte /
        /// get must agree with linear scans.
        #[test]
        fn store_matches_reference(
            init in prop::collection::vec(arb_spec(), 0..30),
            ops in prop::collection::vec(
                (any::<u16>(), any::<u16>(), prop::collection::vec(arb_spec(), 0..4)),
                0..20),
        ) {
            let mut store = {
                let toks = ref_to_tokens(&init);
                TokenStore::from_tokens(&toks)
            };
            let mut model = init.clone();

            for (a, b, new) in ops {
                let n = model.len();
                let lo = if n == 0 { 0 } else { (a as usize) % (n + 1) };
                let hi_raw = if n == 0 { 0 } else { (b as usize) % (n + 1) };
                let (lo, hi) = (lo.min(hi_raw), lo.max(hi_raw));
                store.splice(lo, hi, &new);
                model.splice(lo..hi, new.iter().copied());

                // token sequences (with absolute positions) agree
                prop_assert_eq!(store.to_vec(), ref_to_tokens(&model));
                prop_assert_eq!(store.len(), model.len());
            }

            // index_at_byte agrees with a linear scan over the model
            let toks = ref_to_tokens(&model);
            let total = store.byte_len();
            for byte in 0..=total {
                let want = toks.iter().position(|t| (t.end as u64) > byte).unwrap_or(toks.len());
                prop_assert_eq!(store.index_at_byte(byte), want, "byte {}", byte);
            }
            // get agrees with the model
            for i in 0..model.len() {
                prop_assert_eq!(store.get(i), Some(toks[i]));
            }
            prop_assert_eq!(store.get(model.len()), None);
        }
    }
}
