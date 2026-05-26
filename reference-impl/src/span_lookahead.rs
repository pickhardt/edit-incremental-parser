//! Naive span + N-token lookahead reuse predicate (comparator).
//!
//! For comparison against precedence-bounded. NOT SOUND in general: this
//! is the textbook "production" predicate that doesn't check whether the
//! cached subtree's internal precedence structure is still valid in the
//! new surrounding context.

use rustc_hash::FxHashMap;
use std::sync::Arc;

use crate::ast::Node;
use crate::edit::Edit;
use crate::incremental::{ReparseStats, ReuseCache};
use crate::lexer::{tokenize, Token, TokenKind};
use crate::op::MIN_PREC;
use crate::parser::ParseError;
use crate::pratt_core::PrattCore;

pub fn span_lookahead_parse(
    old_tree: &Arc<Node>,
    old_src: &str,
    edit: &Edit,
    lookahead: usize,
) -> Result<(Arc<Node>, String, ReparseStats), ParseError> {
    let cache = ReuseCache::build(old_tree, old_src);
    span_lookahead_with_cache(&cache, old_src, edit, lookahead)
}

pub fn span_lookahead_with_cache(
    cache: &ReuseCache,
    old_src: &str,
    edit: &Edit,
    lookahead: usize,
) -> Result<(Arc<Node>, String, ReparseStats), ParseError> {
    let new_src = edit.apply(old_src);
    let new_tokens = tokenize(&new_src);
    let old_tokens = tokenize(old_src);

    // For lookahead lookup: map old byte → index in old token stream.
    let mut old_pos_after_byte: FxHashMap<u32, usize> = FxHashMap::default();
    for (i, t) in old_tokens.iter().enumerate() {
        old_pos_after_byte.entry(t.start).or_insert(i);
    }

    // Build a private (start_byte → Arc<Node>) view from the shared
    // cache by walking with old tokens (already done by ReuseCache).
    // We can borrow ReuseCache's internal map indirectly through the
    // helper below since ReuseCache lives in the `incremental` module.
    let by_byte = unsafe_borrow_cache(cache);

    let mut p = SpanLAParser {
        old_src,
        src: &new_src,
        tokens: &new_tokens,
        old_tokens: &old_tokens,
        old_pos_after_byte: &old_pos_after_byte,
        pos: 0,
        edit,
        cache: by_byte,
        lookahead,
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

// ReuseCache is opaque from outside its module; we want to read its
// inner map. Add a public accessor in incremental.rs or use this
// transmute-free shortcut: rebuild from scratch (small extra cost).
fn unsafe_borrow_cache(cache: &ReuseCache) -> &FxHashMap<u32, Vec<Arc<Node>>> {
    cache.inner()
}

struct SpanLAParser<'a> {
    old_src: &'a str,
    src: &'a str,
    tokens: &'a [Token],
    old_tokens: &'a [Token],
    old_pos_after_byte: &'a FxHashMap<u32, usize>,
    pos: usize,
    edit: &'a Edit,
    cache: &'a FxHashMap<u32, Vec<Arc<Node>>>,
    lookahead: usize,
    stats: ReparseStats,
}

impl<'a> PrattCore<'a> for SpanLAParser<'a> {
    fn src(&self) -> &'a str { self.src }
    fn tokens(&self) -> &'a [Token] { self.tokens }
    fn pos(&self) -> usize { self.pos }
    fn set_pos(&mut self, pos: usize) { self.pos = pos; }

    fn on_parsed(&mut self) { self.stats.nodes_parsed += 1; }

    /// Naive span + N-token-lookahead reuse predicate. NOT SOUND in
    /// general — does not check that the cached subtree's internal
    /// precedence structure remains valid in the new surrounding
    /// context. Models the textbook "production" predicate that the
    /// paper benchmarks (§6.2) and finds wrong on ~1–2% of random
    /// adversarial edits (§6.4).
    fn try_reuse(&mut self, _min_prec: u32) -> Option<Arc<Node>> {
        let new_byte = self.peek().start;
        let old_byte = self.new_byte_to_old(new_byte)?;
        let candidates = self.cache.get(&old_byte)?;
        for cand in candidates {
            self.stats.reuse_attempts += 1;
            let old_start = old_byte;
            let old_end = old_start + cand.width;
            let (new_start, new_end) = match self.edit.translate_old_range(old_start, old_end) {
                Some(p) => p,
                None => { self.stats.reuse_rejected_changed += 1; continue; }
            };
            if !self.boundary_bytes_match(old_start, old_end, new_start, new_end) {
                self.stats.reuse_rejected_changed += 1;
                continue;
            }
            let target_pos = match self.find_token_index(new_end) {
                Some(p) => p,
                None => { self.stats.reuse_rejected_changed += 1; continue; }
            };
            if !self.lookahead_matches(old_end, target_pos) {
                self.stats.reuse_rejected_precedence += 1;
                continue;
            }
            self.pos = target_pos;
            self.stats.nodes_reused += cand.count();
            return Some(Arc::clone(cand));
        }
        None
    }
}

impl<'a> SpanLAParser<'a> {
    fn new_byte_to_old(&self, new_byte: u32) -> Option<u32> {
        let e = self.edit;
        let new_edit_end = e.start + e.replacement.len() as u32;
        if new_byte < e.start { Some(new_byte) }
        else if new_byte >= new_edit_end {
            let delta = e.replacement.len() as i64 - (e.end - e.start) as i64;
            Some((new_byte as i64 - delta) as u32)
        } else { None }
    }

    fn boundary_bytes_match(
        &self, old_start: u32, old_end: u32, new_start: u32, new_end: u32,
    ) -> bool {
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

    fn find_token_index(&self, byte: u32) -> Option<usize> {
        for (i, t) in self.tokens[self.pos..].iter().enumerate() {
            if t.start >= byte { return Some(self.pos + i); }
        }
        None
    }

    fn lookahead_matches(&self, old_end: u32, new_pos: usize) -> bool {
        if self.lookahead == 0 { return true; }
        let old_pos = match self.old_pos_after_byte.iter()
            .filter(|(b, _)| **b >= old_end)
            .min_by_key(|(b, _)| **b) {
            Some((_, idx)) => *idx,
            None => return false,
        };
        for k in 0..self.lookahead {
            let oi = old_pos + k;
            let ni = new_pos + k;
            if oi >= self.old_tokens.len() || ni >= self.tokens.len() {
                return oi >= self.old_tokens.len() && ni >= self.tokens.len();
            }
            if self.old_tokens[oi].kind != self.tokens[ni].kind { return false; }
        }
        true
    }

}
