//! Arc-based incremental Pratt parser with precedence-bounded reuse.
//!
//! The reuse predicate (see `ast::Node` docs and Paper §3 for the
//! formal statement):
//!   1. Precedence band: `cand.stop_lbp <= M_new < cand.m_spine`
//!   2. Text region: cached subtree's old byte span outside the edit
//!   3. Tokenization boundary: bytes immediately before and after the
//!      span are unchanged
//!   4. Next-token lbp: the lbp of the new-source token immediately
//!      following the reused span equals `cand.stop_lbp` (closes the
//!      gap when an edit splits a multi-byte operator across
//!      whitespace; see `try_reuse` doc comment for details)
//!
//! Reuse is an `Arc::clone` — a refcount bump, O(1) regardless of subtree
//! size. The cache is precomputed once per loaded tree (`ReuseCache::build`)
//! and reused across many edits.

use std::sync::Arc;

use rustc_hash::FxHashMap;

use crate::ast::{Node, NodeKind};
use crate::edit::Edit;
use crate::lexer::{tokenize, Token, TokenKind};
use crate::op::MIN_PREC;
use crate::parser::ParseError;
use crate::pratt_core::PrattCore;
use crate::recovery::{local_optimal_repair, Repair};

#[derive(Debug, Default, Clone)]
pub struct ReparseStats {
    pub nodes_reused: u32,
    pub nodes_parsed: u32,
    pub reuse_attempts: u32,
    pub reuse_rejected_precedence: u32,
    pub reuse_rejected_changed: u32,
}

/// Precomputed reuse cache. Build once per loaded tree (amortized); reuse
/// across many edits via `incremental_parse_with_cache`.
pub struct ReuseCache {
    /// Candidate-lookup index: **old-source start byte → subtrees that begin
    /// there** (sorted widest-first). Answers "what cached subtrees start at
    /// byte X?" — the lookup `try_reuse` does at each recursion frame. Always
    /// present; this is the core reuse cache.
    by_byte: FxHashMap<u32, Vec<Arc<Node>>>,
    /// Node-span index: **node id → its `(start, end)` in the OLD source**
    /// (old = pre-edit coordinates, not "legacy"). Answers the opposite
    /// question to `by_byte` — "where does *this particular* node live?" —
    /// which the `chain_splice` descent needs to decide whether an edit falls
    /// in a chain node's left or right child. It is a *side table* rather than
    /// a field on `Node` because nodes deliberately store width, not absolute
    /// position (the Roslyn red/green design that makes subtrees shareable);
    /// absolute spans are recomputed here once per cache build. Compiled only
    /// under `chain_splice`, so the default build (and the §5 cache-rebuild
    /// numbers) is unaffected.
    #[cfg(feature = "chain_splice")]
    spans: FxHashMap<crate::ast::NodeId, (u32, u32)>,
}

impl ReuseCache {
    /// Internal map: byte-start → candidate subtrees. Exposed for use
    /// by comparator parsers (`span_lookahead`, `roslyn_style`) that
    /// want to share the same cache representation.
    pub fn inner(&self) -> &FxHashMap<u32, Vec<Arc<Node>>> {
        &self.by_byte
    }

    pub fn build(old_tree: &Arc<Node>, old_src: &str) -> Self {
        let old_tokens = tokenize(old_src);
        let mut by_byte: FxHashMap<u32, Vec<Arc<Node>>> = FxHashMap::default();
        let mut cursor = 0usize;
        walk_cache(old_tree, &mut cursor, &old_tokens, &mut by_byte);
        for v in by_byte.values_mut() {
            v.sort_by_key(|n| std::cmp::Reverse(n.width));
        }
        #[cfg(feature = "chain_splice")]
        let spans = {
            let mut spans: FxHashMap<crate::ast::NodeId, (u32, u32)> = FxHashMap::default();
            let mut c = 0usize;
            walk_spans(old_tree, &mut c, &old_tokens, &mut spans);
            spans
        };
        ReuseCache {
            by_byte,
            #[cfg(feature = "chain_splice")]
            spans,
        }
    }
}

/// Convenience: build cache + reparse. Pays cache-build cost per call.
pub fn incremental_parse(
    old_tree: &Arc<Node>,
    old_src: &str,
    edit: &Edit,
) -> Result<(Arc<Node>, String, ReparseStats), ParseError> {
    let cache = ReuseCache::build(old_tree, old_src);
    incremental_parse_with_cache(&cache, old_src, edit)
}

/// Per-edit hot path: no cache rebuild, no old-source tokenization.
pub fn incremental_parse_with_cache(
    cache: &ReuseCache,
    old_src: &str,
    edit: &Edit,
) -> Result<(Arc<Node>, String, ReparseStats), ParseError> {
    let new_src = edit.apply(old_src);
    let new_tokens = tokenize(&new_src);

    let mut p = IncrementalParser {
        old_src,
        src: &new_src,
        tokens: &new_tokens,
        pos: 0,
        edit,
        cache: &cache.by_byte,
        #[cfg(feature = "chain_splice")]
        spans: &cache.spans,
        stats: ReparseStats::default(),
    };
    let result = (|| -> Result<(Arc<Node>, ReparseStats), ParseError> {
        let node = p.parse_expr(MIN_PREC)?;
        if p.peek().kind != TokenKind::Eof {
            return Err(ParseError::TrailingTokens { at: p.peek().start });
        }
        Ok((node, p.stats.clone()))
    })()?;
    Ok((result.0, new_src, result.1))
}

/// Result of a recovering incremental reparse (never fails).
#[derive(Debug, Clone)]
pub struct IncrementalRecovery {
    pub tree: Arc<Node>,
    pub new_src: String,
    /// Empty when the edit parsed cleanly (the fast reuse path).
    pub repairs: Vec<Repair>,
    /// The precedence-bounded region the repair was confined to; `None` when
    /// the edit parsed cleanly or recovery escalated to the full input.
    pub region: Option<(usize, usize)>,
    pub stats: ReparseStats,
}

/// Incremental reparse that **never fails** (recovery_design.md §5.3). A clean
/// edit takes the fast precedence-bounded *reuse* path; an edit that
/// introduces a syntax error falls through to localized cost-optimal
/// *recovery* — repair confined to the precedence-bounded region around the
/// edit-induced error, with everything outside it left intact. This is
/// recovery *inside* the incremental parser, the gap Diekmann [2019, §3.1]
/// names as unfilled.
///
/// POC scope: on the recovery path the repaired tree is rebuilt by the repair
/// search rather than sharing `Arc` subtrees with the cache — the *search* is
/// region-local, but Arc-reuse during recovery is a further optimization left
/// as future work. Clean edits retain full Arc-reuse.
pub fn incremental_parse_with_cache_recovering(
    cache: &ReuseCache,
    old_src: &str,
    edit: &Edit,
) -> IncrementalRecovery {
    match incremental_parse_with_cache(cache, old_src, edit) {
        Ok((tree, new_src, stats)) => IncrementalRecovery {
            tree,
            new_src,
            repairs: Vec::new(),
            region: None,
            stats,
        },
        Err(_) => {
            let new_src = edit.apply(old_src);
            let local = local_optimal_repair(&new_src);
            IncrementalRecovery {
                tree: local.result.tree,
                new_src,
                repairs: local.result.repairs,
                region: local.region,
                stats: ReparseStats::default(),
            }
        }
    }
}

/// Walk old tree alongside old token stream, recording each subtree's
/// exact absolute start byte.
///
/// Atom nodes are NOT cached: a cache hit for an atom costs a HashMap
/// lookup + Arc::clone + boundary check, while rebuilding fresh is one
/// Arc allocation. Skipping atoms shrinks the cache substantially —
/// expression-heavy sources have many atoms — and makes cache build
/// noticeably faster, with negligible loss of reuse opportunity since
/// every other node type still flows through `try_reuse` and reuses
/// its (atom) children via Arc::clone of the parent.
fn walk_cache(
    node: &Arc<Node>,
    cursor: &mut usize,
    tokens: &[Token],
    cache: &mut FxHashMap<u32, Vec<Arc<Node>>>,
) {
    let start_byte = tokens[*cursor].start;
    // Atoms and recovery markers (Missing/Error) are never cached for
    // reuse. Recovered-tree caching is out of scope until incremental
    // integration (M5 of recovery_design.md).
    let is_atom = matches!(
        node.kind,
        NodeKind::Atom(_) | NodeKind::Missing | NodeKind::Error { .. }
    );
    if !is_atom {
        cache.entry(start_byte).or_default().push(node.clone());
    }
    match &node.kind {
        NodeKind::Atom(_) => {
            *cursor += 1;
        }
        NodeKind::Prefix { child, .. } => {
            *cursor += 1;
            walk_cache(child, cursor, tokens, cache);
        }
        NodeKind::Binary { left, right, .. } => {
            walk_cache(left, cursor, tokens, cache);
            *cursor += 1;
            walk_cache(right, cursor, tokens, cache);
        }
        NodeKind::Ternary { cond, then, else_ } => {
            walk_cache(cond, cursor, tokens, cache);
            *cursor += 1;
            walk_cache(then, cursor, tokens, cache);
            *cursor += 1;
            walk_cache(else_, cursor, tokens, cache);
        }
        NodeKind::Paren { inner } => {
            *cursor += 1;
            walk_cache(inner, cursor, tokens, cache);
            *cursor += 1;
        }
        NodeKind::Call { callee, args } => {
            walk_cache(callee, cursor, tokens, cache);
            *cursor += 1; // `(`
            for (i, arg) in args.iter().enumerate() {
                if i > 0 {
                    *cursor += 1; // `,`
                }
                walk_cache(arg, cursor, tokens, cache);
            }
            *cursor += 1; // `)`
        }
        NodeKind::Index { array, index } => {
            walk_cache(array, cursor, tokens, cache);
            *cursor += 1; // `[`
            walk_cache(index, cursor, tokens, cache);
            *cursor += 1; // `]`
        }
        NodeKind::Member { object, .. } => {
            walk_cache(object, cursor, tokens, cache);
            *cursor += 1; // `.`
            *cursor += 1; // ident
        }
        // Recovery markers do not correspond to a stable old-token span:
        // Missing consumes no token; Error records the tokens it skipped.
        // Neither is cached (see is_atom above); these arms only keep the
        // cursor consistent if a recovered tree is ever walked.
        NodeKind::Missing => {}
        NodeKind::Error { skipped } => {
            *cursor += skipped.len();
        }
    }
}

/// Record every node's OLD-source `(start, end)` span into `spans`, keyed by
/// node id. Mirrors `walk_cache`'s token-cursor accounting exactly (so the
/// `start_byte` it sees for each node matches), but records *all* nodes
/// (including atoms) rather than only the reuse candidates. Built once per
/// cache; consumed by the `chain_splice` descent in `try_splice_chain`.
#[cfg(feature = "chain_splice")]
fn walk_spans(
    node: &Arc<Node>,
    cursor: &mut usize,
    tokens: &[Token],
    spans: &mut FxHashMap<crate::ast::NodeId, (u32, u32)>,
) {
    let start_byte = tokens[*cursor].start;
    spans.insert(node.id, (start_byte, start_byte + node.width));
    match &node.kind {
        NodeKind::Atom(_) => {
            *cursor += 1;
        }
        NodeKind::Prefix { child, .. } => {
            *cursor += 1;
            walk_spans(child, cursor, tokens, spans);
        }
        NodeKind::Binary { left, right, .. } => {
            walk_spans(left, cursor, tokens, spans);
            *cursor += 1;
            walk_spans(right, cursor, tokens, spans);
        }
        NodeKind::Ternary { cond, then, else_ } => {
            walk_spans(cond, cursor, tokens, spans);
            *cursor += 1;
            walk_spans(then, cursor, tokens, spans);
            *cursor += 1;
            walk_spans(else_, cursor, tokens, spans);
        }
        NodeKind::Paren { inner } => {
            *cursor += 1;
            walk_spans(inner, cursor, tokens, spans);
            *cursor += 1;
        }
        NodeKind::Call { callee, args } => {
            walk_spans(callee, cursor, tokens, spans);
            *cursor += 1; // `(`
            for (i, arg) in args.iter().enumerate() {
                if i > 0 {
                    *cursor += 1; // `,`
                }
                walk_spans(arg, cursor, tokens, spans);
            }
            *cursor += 1; // `)`
        }
        NodeKind::Index { array, index } => {
            walk_spans(array, cursor, tokens, spans);
            *cursor += 1; // `[`
            walk_spans(index, cursor, tokens, spans);
            *cursor += 1; // `]`
        }
        NodeKind::Member { object, .. } => {
            walk_spans(object, cursor, tokens, spans);
            *cursor += 1; // `.`
            *cursor += 1; // ident
        }
        NodeKind::Missing => {}
        NodeKind::Error { skipped } => {
            *cursor += skipped.len();
        }
    }
}

struct IncrementalParser<'a> {
    old_src: &'a str,
    src: &'a str,
    tokens: &'a [Token],
    pos: usize,
    edit: &'a Edit,
    cache: &'a FxHashMap<u32, Vec<Arc<Node>>>,
    /// Node-span side table for the chain-splice fast path (see `ReuseCache`).
    #[cfg(feature = "chain_splice")]
    spans: &'a FxHashMap<crate::ast::NodeId, (u32, u32)>,
    stats: ReparseStats,
}

impl<'a> PrattCore<'a> for IncrementalParser<'a> {
    fn src(&self) -> &'a str { self.src }
    fn tokens(&self) -> &'a [Token] { self.tokens }
    fn pos(&self) -> usize { self.pos }
    fn set_pos(&mut self, pos: usize) { self.pos = pos; }

    fn on_parsed(&mut self) { self.stats.nodes_parsed += 1; }

    /// Precedence-bounded reuse predicate (Definition 3.1 of the
    /// paper). Four conditions:
    ///   1. Precedence band: `cand.stop_lbp <= min_prec < cand.m_spine`
    ///   2. Text region: `cand.span` outside the edit
    ///   3. Tokenization boundary: bytes at span's immediate
    ///      neighbors unchanged across the edit
    ///   4. Next-token lbp: the lbp of the new-source token
    ///      immediately following the reused span equals
    ///      `cand.stop_lbp`. Necessary because the lexer skips
    ///      whitespace, so an edit several bytes past the boundary
    ///      can change the *next non-whitespace* token even with
    ///      the immediate boundary byte unchanged (e.g., `||`
    ///      split by a space-insertion edit becomes `| |` — two
    ///      unknown chars, skipped — so the next token is whatever
    ///      follows). Without this check, the §3 paper proof's
    ///      reliance on "boundary bytes unchanged ⇒ next-token
    ///      kind unchanged" fails for multi-byte operators broken
    ///      by whitespace-insertion edits.
    fn try_reuse(&mut self, min_prec: u32) -> Option<Arc<Node>> {
        let candidates = self.cache_candidates()?;
        for cand in candidates {
            self.stats.reuse_attempts += 1;
            if min_prec >= cand.m_spine || min_prec < cand.stop_lbp {
                self.stats.reuse_rejected_precedence += 1;
                continue;
            }
            let old_start = self.new_byte_to_old(self.peek().start).unwrap();
            let old_end = old_start + cand.width;
            let (new_start, new_end) = match self.edit.translate_old_range(old_start, old_end) {
                Some(p) => p,
                None => {
                    // The edit lies inside `cand`. Whole-subtree reuse is out,
                    // but if `cand` is an associativity-conflict chain whose
                    // edit is confined to a single operand, we can splice that
                    // operand in O(log n) instead of falling back to the flat
                    // O(n) rebuild. Sound by the ≈-quotient (Paper 2 §5): the
                    // result is a balanced regrouping of the same operands.
                    #[cfg(feature = "chain_splice")]
                    {
                        // Fast path: in-place single-operand edit (structurally
                        // identical to fresh). Then the general path: operand
                        // insert/delete/operator-change via split + reparse-middle
                        // + rebalanced rebuild (≈-equivalent to fresh).
                        if let Some(spliced) = self.try_splice_chain(cand, old_start, old_end) {
                            return Some(spliced);
                        }
                        if let Some(spliced) = self.try_splice_general(cand, old_start, old_end) {
                            return Some(spliced);
                        }
                    }
                    self.stats.reuse_rejected_changed += 1;
                    continue;
                }
            };
            if !self.boundary_bytes_match(old_start, old_end, new_start, new_end) {
                self.stats.reuse_rejected_changed += 1;
                continue;
            }
            let target_pos = match self.find_token_index(new_end) {
                Some(p) => p,
                None => {
                    self.stats.reuse_rejected_changed += 1;
                    continue;
                }
            };
            // Condition 4: next-token lbp at target_pos must equal
            // cand.stop_lbp (see doc comment above).
            let next_lbp_new = crate::op::lbp(self.tokens[target_pos].kind);
            if next_lbp_new != cand.stop_lbp {
                self.stats.reuse_rejected_changed += 1;
                continue;
            }
            self.pos = target_pos;
            self.stats.nodes_reused += cand.count();
            return Some(Arc::clone(cand));
        }
        None
    }
}

impl<'a> IncrementalParser<'a> {
    fn new_byte_to_old(&self, new_byte: u32) -> Option<u32> {
        let e = self.edit;
        let new_edit_end = e.start + e.replacement.len() as u32;
        if new_byte < e.start {
            Some(new_byte)
        } else if new_byte >= new_edit_end {
            let delta = e.replacement.len() as i64 - (e.end - e.start) as i64;
            Some((new_byte as i64 - delta) as u32)
        } else {
            None
        }
    }

    fn cache_candidates(&self) -> Option<&'a Vec<Arc<Node>>> {
        let new_byte = self.peek().start;
        let old_byte = self.new_byte_to_old(new_byte)?;
        self.cache.get(&old_byte)
    }

    fn boundary_bytes_match(
        &self,
        old_start: u32,
        old_end: u32,
        new_start: u32,
        new_end: u32,
    ) -> bool {
        let old = self.old_src.as_bytes();
        let new = self.src.as_bytes();
        let left_ok = match (old_start == 0, new_start == 0) {
            (true, true) => true,
            (false, false) => old[old_start as usize - 1] == new[new_start as usize - 1],
            _ => false,
        };
        let right_ok = match (
            old_end == old.len() as u32,
            new_end == new.len() as u32,
        ) {
            (true, true) => true,
            (false, false) => old[old_end as usize] == new[new_end as usize],
            _ => false,
        };
        left_ok && right_ok
    }

    fn find_token_index(&self, byte: u32) -> Option<usize> {
        for (i, t) in self.tokens[self.pos..].iter().enumerate() {
            if t.start >= byte {
                return Some(self.pos + i);
            }
        }
        None
    }

    /// O(log n) chain splice for an edit confined to a single operand of a
    /// cached associativity-conflict chain. `cand` is the chain root the edit
    /// lies inside; `(c_start, c_end)` is its OLD-source span. Returns the new
    /// chain root and advances `pos` past the whole chain, or `None` (no state
    /// change) for any case it can't prove safe — the caller then falls back to
    /// the flat O(n) rebuild.
    ///
    /// ## Why this is O(log n) — and why the naive path is O(n)
    ///
    /// The chain is a *balanced* binary tree (`build_balanced`), so its depth is
    /// O(log k) in the operand count k. An in-place operand edit changes exactly
    /// one leaf. Every node *not* on the root→operand path is structurally
    /// unchanged, so we share it wholesale with one `Arc::clone` (a refcount
    /// bump, O(1), no walk). Only the O(log k) ancestors on the path need fresh
    /// nodes — their `width` shifts by the edit delta but their children are the
    /// reparsed operand on one side and a shared subtree on the other. So the
    /// total work is: O(log k) descent (locate the operand via the span table)
    /// + reparse one operand + O(log k) path-copy = **O(log k)** plus the
    /// operand's own size, independent of chain length.
    ///
    /// The flat fallback (`parse_assoc_chain` + `build_balanced`) instead
    /// re-collects *all* k operands and rebuilds *all* k−1 interior nodes from
    /// scratch every edit — O(k) — even though the unedited operands are reused
    /// individually. That O(k) interior rebuild is exactly what this fast path
    /// removes (see `flat_chain_edit_locality` in tests/semantics.rs: ~204 → ≤48
    /// recomputed for k=200).
    ///
    /// ## Soundness
    ///
    /// The spliced tree is a balanced regrouping of the *same* operand sequence
    /// (only one operand's content changed), so it equals a fresh parse up to
    /// associativity-conflict reassociation (Paper 2 §5's ≈-quotient). In fact
    /// it is *structurally identical* to fresh here: `build_balanced`'s shape
    /// depends only on operand count/order, which an in-place operand edit
    /// leaves unchanged — so even strict `unparse()` agrees (validated by the
    /// parse-equivalence proptest with `chain_splice` on). The operand-extent
    /// guard (`new_o_start + o_new.width == new_o_end`) rejects any edit that
    /// would alter the operand boundaries (insert/delete/operator change),
    /// routing those to the sound flat rebuild.
    #[cfg(feature = "chain_splice")]
    fn try_splice_chain(
        &mut self,
        cand: &Arc<Node>,
        c_start: u32,
        c_end: u32,
    ) -> Option<Arc<Node>> {
        use crate::ast::NodeKind;

        let chain_op = match &cand.kind {
            NodeKind::Binary { op, .. } if crate::op::is_associativity_conflict(*op) => *op,
            _ => return None,
        };
        let (e_lo, e_hi) = (self.edit.start, self.edit.end);
        // Edit must be strictly interior to the chain span.
        if !(c_start < e_lo && e_hi < c_end) {
            return None;
        }

        // Descend the balanced chain to the operand containing the edit,
        // recording the (ancestor, went_left) path. Read-only — no parser
        // state is touched, so any early `return None` here is side-effect free.
        let mut cur: Arc<Node> = Arc::clone(cand);
        let mut path: Vec<(Arc<Node>, bool)> = Vec::new();
        loop {
            let (go_left, next) = match &cur.kind {
                NodeKind::Binary { op, left, right } if *op == chain_op => {
                    let (ls, le) = *self.spans.get(&left.id)?;
                    let (rs, re) = *self.spans.get(&right.id)?;
                    if e_lo >= ls && e_hi <= le {
                        (true, Arc::clone(left))
                    } else if e_lo >= rs && e_hi <= re {
                        (false, Arc::clone(right))
                    } else {
                        // Edit straddles the operator/gap or both operands.
                        return None;
                    }
                }
                // Not a chain-spine node: this is the operand containing the edit.
                _ => break,
            };
            path.push((Arc::clone(&cur), go_left));
            cur = next;
        }
        let operand_old = cur;
        let (o_start, o_end) = *self.spans.get(&operand_old.id)?;
        // Edit must be strictly interior to the operand (unchanged bytes flank
        // it on both sides), so the operand's start byte is unedited and its
        // end merely shifts by the edit delta.
        if !(o_start < e_lo && e_hi < o_end) {
            return None;
        }

        let new_o_start = o_start; // bytes <= edit.start are unchanged
        let new_o_end = self.edit.map_old_to_new(o_end)?; // o_end > e_hi ⇒ shifted

        // --- Speculative reparse of just the operand; restore on any mismatch. ---
        let saved_pos = self.pos;
        let saved_stats = self.stats.clone();

        let start_tok = self.find_token_index(new_o_start)?;
        if self.tokens[start_tok].start != new_o_start {
            return None; // operand start no longer on a token boundary
        }
        self.pos = start_tok;
        let o_new = match self.parse_expr(crate::op::rbp(chain_op)) {
            Ok(n) => n,
            Err(_) => {
                self.pos = saved_pos;
                self.stats = saved_stats;
                return None;
            }
        };
        // The reparsed operand must occupy exactly its translated old extent:
        // start unchanged (`new_o_start`) and end at `new_o_end`. We check the
        // operand's own width (not the next token's start, which is separated
        // by the operator + whitespace). A mismatch means the edit changed the
        // chain's operand structure (grew/shrank the operand, or altered an
        // operator), so a single-leaf splice would be unsound — fall back.
        if new_o_start + o_new.width != new_o_end {
            self.pos = saved_pos;
            self.stats = saved_stats;
            return None;
        }

        // --- Path-copy: rebuild only the O(log n) ancestors to the chain root.
        // Each iteration allocates ONE new spine node and shares the off-path
        // sibling subtree with a single `Arc::clone` (refcount bump, no walk).
        // `path` has length O(log k) because the chain tree is balanced, so the
        // whole loop is O(log k) regardless of how many operands the chain has.
        let delta: i64 = o_new.width as i64 - operand_old.width as i64;
        let reused = cand.count().saturating_sub(operand_old.count());
        let mut node = o_new;
        for (ancestor, went_left) in path.into_iter().rev() {
            let (l, r) = match &ancestor.kind {
                // The kept sibling is shared, not rebuilt: O(1) per level.
                NodeKind::Binary { left, right, .. } => (Arc::clone(left), Arc::clone(right)),
                _ => unreachable!("path nodes are chain-spine Binary nodes"),
            };
            let (nl, nr) = if went_left { (node, r) } else { (l, node) };
            node = Arc::new(Node {
                kind: NodeKind::Binary { op: chain_op, left: nl, right: nr },
                width: (ancestor.width as i64 + delta) as u32,
                m_spine: ancestor.m_spine,
                stop_lbp: ancestor.stop_lbp,
                m_floor: ancestor.m_floor,
                id: crate::ast::fresh_id(),
                // In-place edit: operand count unchanged, so wb is preserved.
                wb: ancestor.wb,
            });
        }

        // Advance the cursor past the whole chain in the new token stream.
        let chain_end_new = self.edit.map_old_to_new(c_end)?;
        let after = self.find_token_index(chain_end_new)?;
        self.pos = after;
        self.stats.nodes_reused += reused;
        Some(node)
    }

    /// Collect the chain's maximal off-path subtree "units": every subtree of
    /// `node` whose OLD span is entirely before the edit goes to `left` (in
    /// source order), entirely after goes to `right`. Operands overlapping the
    /// edit are dropped (they are the "middle", reparsed by the caller). There
    /// are O(log k) units because the chain tree is balanced and the recursion
    /// follows only the path(s) to the edit. Returns `false` if a span lookup
    /// misses (caller falls back). Each unit is `(subtree, old_start_byte)`.
    #[cfg(feature = "chain_splice")]
    fn collect_chain_units(
        &self,
        node: &Arc<Node>,
        e_lo: u32,
        e_hi: u32,
        chain_op: TokenKind,
        left: &mut Vec<(Arc<Node>, u32)>,
        right: &mut Vec<(Arc<Node>, u32)>,
    ) -> bool {
        let (s, e) = match self.spans.get(&node.id) {
            Some(&p) => p,
            None => return false,
        };
        // STRICT separation. A unit whose boundary touches the edit (`e == e_lo`
        // or `s == e_hi`) must NOT be reused: an edit adjacent to its last/first
        // token can merge with it (e.g. operand `30` + insert `42` at its end ⇒
        // `3042`). Requiring a separating unchanged byte means the unit's
        // maximal token cannot continue across the edit, so its tokens are
        // genuinely unchanged. Touching operands fall into the reparsed middle.
        if e < e_lo {
            left.push((Arc::clone(node), s)); // strictly before the edit
            return true;
        }
        if s > e_hi {
            right.push((Arc::clone(node), s)); // strictly after the edit
            return true;
        }
        // Overlaps the edit: recurse if it's a chain-spine node, else it's an
        // operand straddling the edit — drop it (the middle reparse covers it).
        match &node.kind {
            NodeKind::Binary { op, left: l, right: r } if *op == chain_op => {
                self.collect_chain_units(l, e_lo, e_hi, chain_op, left, right)
                    && self.collect_chain_units(r, e_lo, e_hi, chain_op, left, right)
            }
            _ => true,
        }
    }

    /// General O(log k) chain splice for operand insert / delete / operator
    /// change (the cases the in-place `try_splice_chain` rejects). Strategy:
    /// keep the O(log k) off-path subtrees flanking the edit (`Arc::clone`d,
    /// ids preserved → reused by downstream passes), reparse only the changed
    /// middle operands, and rebuild a balanced spine over
    /// `[left units ++ middle ++ right units]` with the existing
    /// `build_balanced`. Because the unit list is O(log k) long, the rebuilt
    /// spine is O(log k) deep and the total work per edit is O(log k + changed).
    ///
    /// Note: this is O(log k) *per edit* and sound, but it is NOT optimally
    /// self-balancing across many edits — `build_balanced` splits the unit list
    /// by count, not size, so a large off-path unit can sink one level deeper
    /// each time and depth creeps up (measured: 9 → 17 over 40 edits, → 19 over
    /// 200, plateauing — `chain_splice_depth_across_many_edits`). Optimal
    /// sustained balance would need a weight-balanced / 2-3-tree rebalance; the
    /// in-place `try_splice_chain` path, by contrast, is exactly depth-preserving.
    ///
    /// Soundness: the result is a balanced regrouping of the SAME operand
    /// sequence in source order (units are subtrees of the old chain in order;
    /// the middle is the reparse of exactly the changed span). It therefore
    /// equals a fresh parse up to associativity-conflict reassociation
    /// (Paper 2 §5's ≈-quotient). `unparse_normalized` — which flattens the
    /// chain, recursively expanding the multi-operand units back into the same
    /// flat operand list a fresh parse produces — agrees, and so does eval
    /// (associative value is grouping-invariant). The width sanity check and
    /// the chain-op boundary checks reject any case it can't reassemble
    /// exactly, routing it to the sound flat O(n) rebuild.
    #[cfg(feature = "chain_splice")]
    fn try_splice_general(
        &mut self,
        cand: &Arc<Node>,
        c_start: u32,
        c_end: u32,
    ) -> Option<Arc<Node>> {
        use crate::parser::ChainOperand;

        let chain_op = match &cand.kind {
            NodeKind::Binary { op, .. } if crate::op::is_associativity_conflict(*op) => *op,
            _ => return None,
        };
        let (e_lo, e_hi) = (self.edit.start, self.edit.end);
        // Edit strictly interior to the chain (boundary edits fall back).
        if !(c_start < e_lo && e_hi < c_end) {
            return None;
        }

        let mut left: Vec<(Arc<Node>, u32)> = Vec::new();
        let mut right: Vec<(Arc<Node>, u32)> = Vec::new();
        if !self.collect_chain_units(cand, e_lo, e_hi, chain_op, &mut left, &mut right) {
            return None;
        }

        let delta: i64 = self.edit.replacement.len() as i64 - (e_hi - e_lo) as i64;
        // Middle byte region in NEW source: from just after the last left unit
        // (unchanged position) to just before the first right unit (shifted).
        let mid_new_start = match left.last() {
            Some((n, s)) => s + n.width, // left unit end (<= e_lo, unchanged)
            None => c_start,
        };
        let mid_new_end = match right.first() {
            Some((_, s)) => (*s as i64 + delta) as u32, // right start (>= e_hi, shifted)
            None => (c_end as i64 + delta) as u32,
        };

        let saved_pos = self.pos;
        let saved_stats = self.stats.clone();
        macro_rules! bail {
            () => {{
                self.pos = saved_pos;
                self.stats = saved_stats;
                return None;
            }};
        }

        // Reparse the middle operands.
        let start_tok = match self.find_token_index(mid_new_start) {
            Some(t) => t,
            None => bail!(),
        };
        self.pos = start_tok;
        if !left.is_empty() {
            // Skip the chain operator joining the last left unit to the middle.
            if self.peek().kind != chain_op {
                bail!();
            }
            self.advance();
        }
        let mut middle: Vec<ChainOperand> = Vec::new();
        while self.peek().start < mid_new_end {
            let opstart = self.peek().start;
            let operand = match self.parse_expr(crate::op::rbp(chain_op)) {
                Ok(n) => n,
                Err(_) => bail!(),
            };
            middle.push(ChainOperand { start_byte: opstart, node: operand });
            if self.peek().kind == chain_op {
                self.advance(); // skip operator; next operand is middle or a right unit
            } else {
                break;
            }
        }

        // Right-boundary guard. The middle must end exactly at the first right
        // unit, reached via a consumed `chain_op` connector. Two ways this can
        // fail, both unsound to splice (fall back to the flat rebuild):
        //   * position mismatch — the edit re-bound a following operand, e.g.
        //     inserting `b *` before `c` pulls `c` into `b * c`;
        //   * the connector is not `chain_op` — e.g. editing `- 1 + - 81` into
        //     `- 142 - 81` destroys the `+`, so the cached `(-81)` is now the
        //     right operand of a binary `-`, not an independent `+` operand.
        //     Requiring `tokens[pos-1] == chain_op` (the operator we consumed to
        //     reach the right unit) catches this. (The left boundary is guarded
        //     above by the leading-operator check.)
        if let Some((_, rs)) = right.first() {
            let first_right_new = (*rs as i64 + delta) as u32;
            if self.peek().start != first_right_new
                || self.pos == 0
                || self.tokens[self.pos - 1].kind != chain_op
            {
                bail!();
            }
        }

        // Assemble units in source order and rebuild a balanced spine.
        let mut units: Vec<ChainOperand> = Vec::with_capacity(left.len() + middle.len() + right.len());
        for (n, s) in &left {
            units.push(ChainOperand { start_byte: *s, node: Arc::clone(n) });
        }
        units.append(&mut middle);
        for (n, s) in &right {
            units.push(ChainOperand { start_byte: (*s as i64 + delta) as u32, node: Arc::clone(n) });
        }
        if units.len() < 2 {
            // A chain must have ≥ 2 operands; fewer means we mis-sliced.
            bail!();
        }
        let result = crate::chain_wb::join_all(chain_op, &units, crate::op::lbp(chain_op));

        // Sanity: the rebuilt chain must span exactly the (shifted) chain bytes.
        let expect_width = ((c_end as i64) + delta - c_start as i64) as u32;
        if result.width != expect_width {
            bail!();
        }

        let after = match self.find_token_index((c_end as i64 + delta) as u32) {
            Some(t) => t,
            None => bail!(),
        };
        self.pos = after;
        let reused: u32 = left.iter().map(|(n, _)| n.count()).sum::<u32>()
            + right.iter().map(|(n, _)| n.count()).sum::<u32>();
        self.stats.nodes_reused += reused;
        Some(result)
    }
}
