//! Incremental parser with precedence-bounded reuse. GENERATED-CRATE
//! RUNTIME (grammar-independent).
//!
//! The four-part reuse predicate (Definition 3.3 of the foundations paper):
//!   1. Precedence band:        `cand.stop_lbp <= M_new < cand.m_spine`
//!   2. Text-region disjoint:   cached span outside the edit
//!   3. Tokenization boundary:  bytes immediately before/after unchanged
//!   4. Next-token lbp:         lbp of the new-source token following the
//!                              reused span equals `cand.stop_lbp`
//!
//! Reuse is an `Arc::clone` — O(1) regardless of subtree size. Lexing is
//! demand-driven: the parser pulls tokens from a `Lexer` cursor, and on a
//! reuse it repositions the cursor past the reused subtree, so the subtree's
//! interior is never lexed. Lexing work therefore tracks the reparsed
//! region, not the file size.

use std::sync::Arc;

use rustc_hash::FxHashMap;

use crate::ast::{Node, NodeKind};
use crate::cursor::{tokenize, Lexer};
use crate::edit::Edit;
use crate::lexer::{lex_token, Token, TokenKind};
use crate::op::MIN_PREC;
use crate::parser::ParseError;
use crate::pratt_core::PrattCore;

#[derive(Debug, Default, Clone)]
pub struct ReparseStats {
    pub nodes_reused: u32,
    pub nodes_parsed: u32,
    pub reuse_attempts: u32,
    pub reuse_rejected_precedence: u32,
    pub reuse_rejected_changed: u32,
    /// Tokens lexed during the reparse (demand-driven). For a local edit on
    /// a reuse-rich source this is far below the token count of the file.
    pub tokens_lexed: u32,
}

/// Precomputed reuse cache: byte-start -> candidate subtrees (largest first).
pub struct ReuseCache {
    by_byte: FxHashMap<u32, Vec<Arc<Node>>>,
}

impl ReuseCache {
    pub fn inner(&self) -> &FxHashMap<u32, Vec<Arc<Node>>> {
        &self.by_byte
    }

    /// Build once per loaded tree (amortized): tokenize the old source and
    /// walk the old tree to record each non-atom subtree's start byte.
    pub fn build(old_tree: &Arc<Node>, old_src: &str) -> Self {
        let old_tokens = tokenize(old_src);
        let mut by_byte: FxHashMap<u32, Vec<Arc<Node>>> = FxHashMap::default();
        let mut cursor = 0usize;
        walk_cache(old_tree, &mut cursor, &old_tokens, &mut by_byte);
        for v in by_byte.values_mut() {
            v.sort_by_key(|n| std::cmp::Reverse(n.width));
        }
        ReuseCache { by_byte }
    }
}

/// Build cache + reparse (pays cache-build cost per call).
pub fn incremental_parse(
    old_tree: &Arc<Node>,
    old_src: &str,
    edit: &Edit,
) -> Result<(Arc<Node>, String, ReparseStats), ParseError> {
    let cache = ReuseCache::build(old_tree, old_src);
    incremental_parse_with_cache(&cache, old_src, edit)
}

/// Per-edit path: apply the edit, then reparse. The string `apply` is O(n)
/// and is editor infrastructure (the editor owns the buffer); see
/// `incremental_reparse` for the parser+lexer hot path alone.
pub fn incremental_parse_with_cache(
    cache: &ReuseCache,
    old_src: &str,
    edit: &Edit,
) -> Result<(Arc<Node>, String, ReparseStats), ParseError> {
    let new_src = edit.apply(old_src);
    let (node, stats) = incremental_reparse(cache, old_src, &new_src, edit)?;
    Ok((node, new_src, stats))
}

/// The parser+lexer hot path: reparse `new_src` (already produced by the
/// editor) given the previous tree's cache and the edit. Lexing is
/// demand-driven, so this does NOT tokenize the whole file.
pub fn incremental_reparse(
    cache: &ReuseCache,
    old_src: &str,
    new_src: &str,
    edit: &Edit,
) -> Result<(Arc<Node>, ReparseStats), ParseError> {
    let mut p = IncrementalParser {
        old_src,
        src: new_src,
        lexer: Lexer::new(new_src),
        edit,
        cache: &cache.by_byte,
        stats: ReparseStats::default(),
    };
    let node = p.parse_expr(MIN_PREC)?;
    if p.peek().kind != TokenKind::Eof {
        return Err(ParseError::TrailingTokens { at: p.peek().start });
    }
    p.stats.tokens_lexed = p.lexer.lexed();
    Ok((node, p.stats))
}

/// Walk old tree alongside old token stream, recording each non-atom
/// subtree's absolute start byte. Atoms are not cached (a fresh atom is
/// cheaper than a cache hit, and atoms ride along inside reused parents).
fn walk_cache(
    node: &Arc<Node>,
    cursor: &mut usize,
    tokens: &[Token],
    cache: &mut FxHashMap<u32, Vec<Arc<Node>>>,
) {
    let start_byte = tokens[*cursor].start;
    let is_atom = matches!(node.kind, NodeKind::Atom(_));
    if !is_atom {
        cache.entry(start_byte).or_default().push(node.clone());
    }
    match &node.kind {
        NodeKind::Atom(_) => {
            *cursor += 1;
        }
        NodeKind::Prefix { child, .. } => {
            *cursor += 1; // prefix operator token
            walk_cache(child, cursor, tokens, cache);
        }
        NodeKind::Postfix { child, .. } => {
            walk_cache(child, cursor, tokens, cache);
            *cursor += 1; // postfix operator token
        }
        NodeKind::Binary { left, right, .. } => {
            walk_cache(left, cursor, tokens, cache);
            *cursor += 1; // infix operator token
            walk_cache(right, cursor, tokens, cache);
        }
        NodeKind::Paren { inner } => {
            *cursor += 1; // open paren
            walk_cache(inner, cursor, tokens, cache);
            *cursor += 1; // close paren
        }
    }
}

struct IncrementalParser<'a> {
    old_src: &'a str,
    src: &'a str,
    lexer: Lexer<'a>,
    edit: &'a Edit,
    cache: &'a FxHashMap<u32, Vec<Arc<Node>>>,
    stats: ReparseStats,
}

impl<'a> PrattCore<'a> for IncrementalParser<'a> {
    fn src(&self) -> &'a str { self.src }
    fn lexer(&self) -> &Lexer<'a> { &self.lexer }
    fn lexer_mut(&mut self) -> &mut Lexer<'a> { &mut self.lexer }
    fn on_parsed(&mut self) {
        self.stats.nodes_parsed += 1;
    }

    fn try_reuse(&mut self, min_prec: u32) -> Option<Arc<Node>> {
        let here = self.peek().start;
        let candidates = self.cache_candidates(here)?;
        for cand in candidates {
            self.stats.reuse_attempts += 1;
            // Condition 1: precedence band.
            if min_prec >= cand.m_spine || min_prec < cand.stop_lbp {
                self.stats.reuse_rejected_precedence += 1;
                continue;
            }
            let old_start = self.new_byte_to_old(here).unwrap();
            let old_end = old_start + cand.width;
            // Condition 2: text-region disjoint (translate unchanged range).
            let (new_start, new_end) = match self.edit.translate_old_range(old_start, old_end) {
                Some(p) => p,
                None => {
                    self.stats.reuse_rejected_changed += 1;
                    continue;
                }
            };
            // Condition 3: tokenization-boundary bytes unchanged.
            if !self.boundary_bytes_match(old_start, old_end, new_start, new_end) {
                self.stats.reuse_rejected_changed += 1;
                continue;
            }
            // Condition 4: lbp of the next token at new_end equals stop_lbp.
            // Lex one token without committing the cursor.
            let next = lex_token(self.src.as_bytes(), new_end as usize);
            if crate::op::lbp(next.kind) != cand.stop_lbp {
                self.stats.reuse_rejected_changed += 1;
                continue;
            }
            // Commit: jump the lexer past the reused subtree.
            self.lexer.reposition(new_end);
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

    fn cache_candidates(&self, new_byte: u32) -> Option<&'a Vec<Arc<Node>>> {
        let old_byte = self.new_byte_to_old(new_byte)?;
        self.cache.get(&old_byte)
    }

    fn boundary_bytes_match(&self, old_start: u32, old_end: u32, new_start: u32, new_end: u32) -> bool {
        let old = self.old_src.as_bytes();
        let new = self.src.as_bytes();
        let left_ok = match (old_start == 0, new_start == 0) {
            (true, true) => true,
            (false, false) => old[old_start as usize - 1] == new[new_start as usize - 1],
            _ => false,
        };
        let right_ok = match (old_end == old.len() as u32, new_end == new.len() as u32) {
            (true, true) => true,
            (false, false) => old[old_end as usize] == new[new_end as usize],
            _ => false,
        };
        left_ok && right_ok
    }
}
