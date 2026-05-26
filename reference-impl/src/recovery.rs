//! Cost-optimal error recovery for the Pratt parser (recovery_design.md).
//!
//! Replaces the `verification` `#[trusted]` stub with a real scheme. This is
//! Milestone M1: the repair model, cost model, and a `recover_parse`
//! entry point that is **bit-identical to fresh `parse` on valid input**
//! (the clean-input invariant). The cost-optimal repair *search* for
//! broken input lands in M2 (`global_optimal_repair`) and M3
//! (`local_optimal_repair` + escalation); until then broken input takes a
//! clearly-marked non-optimal fallback.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};
use std::sync::Arc;

use crate::ast::Node;
use crate::lexer::{tokenize, Token, TokenKind};
use crate::op::{lbp, MIN_PREC};
use crate::pratt_core::PrattCore;

/// Repair cost in the min-plus tropical semiring `(Cost ∪ {∞}, min, +)`.
/// Costs must be non-negative (required by the Theorem 4.2 inequality
/// `cost(global) ≥ cost(inside)`; see recovery_design.md §6).
pub type Cost = u32;

/// Absorbing element for `+`. `u32::MAX / 2` so accumulation never overflows.
pub const INFINITY: Cost = u32::MAX / 2;

/// A token-stream-level repair. `at` indexes the post-lex token stream,
/// not bytes — the lexer silently drops unknown bytes, so byte-garbage
/// never reaches the parser as a token (recovery_design.md §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Repair {
    /// Synthesize a token before stream index `at` (a missing operand,
    /// closer, separator, or operator).
    Insert { at: usize, tok: TokenKind },
    /// Drop the token at stream index `at`.
    Delete { at: usize },
    /// Retype the token at stream index `at` to `to`.
    Substitute { at: usize, to: TokenKind },
}

/// Per-operation repair costs. Default is uniform `1` (matches the old
/// stub and what we verify/benchmark against first). Non-uniform costs
/// are a knob; the theorem needs only non-negativity.
#[derive(Debug, Clone, Copy)]
pub struct RepairCosts {
    pub insert: Cost,
    pub delete: Cost,
    pub substitute: Cost,
}

impl Default for RepairCosts {
    fn default() -> Self {
        RepairCosts { insert: 1, delete: 1, substitute: 1 }
    }
}

impl RepairCosts {
    pub fn cost(&self, r: &Repair) -> Cost {
        match r {
            Repair::Insert { .. } => self.insert,
            Repair::Delete { .. } => self.delete,
            Repair::Substitute { .. } => self.substitute,
        }
    }

    /// Total min-plus cost of a repair sequence: the additive (monoid)
    /// combine. Saturating to keep below `INFINITY`.
    pub fn total(&self, repairs: &[Repair]) -> Cost {
        repairs
            .iter()
            .fold(0, |acc, r| acc.saturating_add(self.cost(r)).min(INFINITY))
    }
}

/// The result of a cost-optimal repair search.
#[derive(Debug, Clone)]
pub struct RepairResult {
    /// Tree of the repaired token stream (materialized via the trusted
    /// `parse`). Synthesized operands appear as canonical atoms.
    pub tree: Arc<Node>,
    /// The minimum-cost repairs, relative to the original token stream.
    pub repairs: Vec<Repair>,
    /// `repair_costs.total(&repairs)`. The min-plus cost.
    pub cost: Cost,
}

/// Canonical repair alphabet: the token kinds the search may insert or
/// substitute to. Covers every one of the seven error sites
/// (recovery_design.md §3). Operators other than `Plus` are omitted: they
/// change *which* operator results, never whether the stream parses or
/// the repair cost, so this set is sound for cost-optimality. Order is the
/// tie-break preference (earlier = preferred).
const ALPHABET: [TokenKind; 6] = [
    TokenKind::Int,
    TokenKind::Ident,
    TokenKind::Plus,
    TokenKind::RParen,
    TokenKind::RBracket,
    TokenKind::Colon,
];

/// Cost bound for the search frontier. Repairs in the corpus are cost 1–2;
/// this bounds the otherwise-exponential oracle and guarantees termination.
pub const MAX_REPAIR_COST: Cost = 6;

/// Recovering parse: **never fails**. On valid input it is identical to
/// `parser::parse` and returns an empty repair list (the clean-input
/// invariant). On broken input it returns the cost-optimal repair's tree
/// plus the repairs that produced it.
pub fn recover_parse(src: &str) -> (Arc<Node>, Vec<Repair>) {
    match crate::parser::parse(src) {
        Ok(tree) => (tree, Vec::new()),
        Err(_) => {
            let r = global_optimal_repair(src);
            (r.tree, r.repairs)
        }
    }
}

/// Globally cost-optimal repair: searches the **entire** token stream.
/// Slow by design — this is the correctness oracle the local path (M3) is
/// validated against (M4). Falls back to an `Error` node wrapping the
/// whole input if no repair exists within `MAX_REPAIR_COST`.
pub fn global_optimal_repair(src: &str) -> RepairResult {
    global_optimal_repair_bounded(src, MAX_REPAIR_COST)
}

/// `global_optimal_repair` with an explicit cost bound. A lower bound makes
/// the bounded search dramatically cheaper on hard/unrepairable inputs
/// (frontier ~ branching^bound), so tests can stay fast; production uses
/// `MAX_REPAIR_COST`.
pub fn global_optimal_repair_bounded(src: &str, max_cost: Cost) -> RepairResult {
    let tokens = tokenize(src);
    optimal_repair(&tokens, src, None, &RepairCosts::default(), max_cost)
        .unwrap_or_else(|| fallback_result(src))
}

/// Cost-optimal repair within an allowed edit window over the original
/// token indices (`None` = the whole stream). Bounded Dijkstra over the
/// min-plus semiring: states are candidate token sequences, edges are
/// repairs, the goal is a sequence that parses cleanly. Returns `None` if
/// no repair of cost ≤ `max_cost` makes the stream parse.
///
/// `tokens` is the full tokenization (including the trailing `Eof`, which
/// is excluded from the editable stream). `window`, when set, is an
/// inclusive-start/exclusive-end range of original token indices; repairs
/// may only touch positions inside it (used by the local path, M3).
pub fn optimal_repair(
    tokens: &[Token],
    src: &str,
    window: Option<(usize, usize)>,
    costs: &RepairCosts,
    max_cost: Cost,
) -> Option<RepairResult> {
    let real = &tokens[..tokens.len().saturating_sub(1)]; // drop trailing Eof
    let in_idx = |orig: usize| window.map_or(true, |(s, e)| s <= orig && orig < e);
    let in_gap = |at: usize| window.map_or(true, |(s, e)| s <= at && at <= e);

    let start_slots: Vec<Slot> = (0..real.len()).map(Slot::Keep).collect();
    let mut heap = BinaryHeap::new();
    heap.push(HeapItem {
        key: (0, 0, Vec::new()),
        slots: start_slots,
        repairs: Vec::new(),
    });
    let mut visited: HashSet<String> = HashSet::new();

    while let Some(item) = heap.pop() {
        let rendered = render(&item.slots, real, src);
        if !visited.insert(rendered.clone()) {
            continue;
        }
        let (cost, _ndel, _sig) = &item.key;
        let cost = *cost;
        if let Ok(tree) = crate::parser::parse(&rendered) {
            return Some(RepairResult { tree, repairs: item.repairs, cost });
        }
        if cost >= max_cost {
            continue;
        }
        // Successors: delete / substitute a kept token; insert a synth token.
        for (i, slot) in item.slots.iter().enumerate() {
            if let Slot::Keep(orig) = *slot {
                if !in_idx(orig) {
                    continue;
                }
                // Delete
                push_succ(&mut heap, &item, costs, i, None, Repair::Delete { at: orig }, max_cost);
                // Substitute
                for &tok in ALPHABET.iter() {
                    push_succ(
                        &mut heap, &item, costs, i, Some(Slot::Synth(tok)),
                        Repair::Substitute { at: orig, to: tok }, max_cost,
                    );
                }
            }
        }
        // Insert at each gap.
        for gap in 0..=item.slots.len() {
            let at = next_keep_orig(&item.slots[gap..], real.len());
            if !in_gap(at) {
                continue;
            }
            for &tok in ALPHABET.iter() {
                let mut slots = item.slots.clone();
                slots.insert(gap, Slot::Synth(tok));
                push_state(
                    &mut heap, &item, costs, slots,
                    Repair::Insert { at, tok }, max_cost,
                );
            }
        }
    }
    None
}

/// Original token index of the first `Keep` slot in `rest`, or `default`.
fn next_keep_orig(rest: &[Slot], default: usize) -> usize {
    for s in rest {
        if let Slot::Keep(orig) = *s {
            return orig;
        }
    }
    default
}

/// Push a successor that replaces slot `i` (`Some(new)`) or deletes it
/// (`None`).
fn push_succ(
    heap: &mut BinaryHeap<HeapItem>,
    item: &HeapItem,
    costs: &RepairCosts,
    i: usize,
    new: Option<Slot>,
    repair: Repair,
    max_cost: Cost,
) {
    let mut slots = item.slots.clone();
    match new {
        Some(s) => slots[i] = s,
        None => {
            slots.remove(i);
        }
    }
    push_state(heap, item, costs, slots, repair, max_cost);
}

/// Push a successor state with one additional `repair` applied.
fn push_state(
    heap: &mut BinaryHeap<HeapItem>,
    item: &HeapItem,
    costs: &RepairCosts,
    slots: Vec<Slot>,
    repair: Repair,
    max_cost: Cost,
) {
    let new_cost = item.key.0.saturating_add(costs.cost(&repair)).min(INFINITY);
    if new_cost > max_cost {
        return;
    }
    let mut repairs = item.repairs.clone();
    repairs.push(repair);
    let ndel = repairs.iter().filter(|r| matches!(r, Repair::Delete { .. })).count();
    let sig: Vec<(u8, usize, u8)> = repairs.iter().map(repair_rank).collect();
    heap.push(HeapItem { key: (new_cost, ndel, sig), slots, repairs });
}

/// Tie-break rank of a repair: (op-class, position, alphabet-rank).
/// Insert < Substitute < Delete; earlier alphabet entry preferred.
fn repair_rank(r: &Repair) -> (u8, usize, u8) {
    match r {
        Repair::Insert { at, tok } => (0, *at, alpha_rank(*tok)),
        Repair::Substitute { at, to } => (1, *at, alpha_rank(*to)),
        Repair::Delete { at } => (2, *at, 0),
    }
}

fn alpha_rank(tok: TokenKind) -> u8 {
    ALPHABET.iter().position(|&t| t == tok).map_or(u8::MAX, |p| p as u8)
}

/// Render a candidate slot sequence to a canonical, lexable source string.
/// Kept tokens render with their original source text; synthesized tokens
/// render with canonical text. Slots are space-separated so adjacent
/// tokens never merge during re-lexing.
fn render(slots: &[Slot], real: &[Token], src: &str) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(slots.len());
    for s in slots {
        parts.push(match s {
            Slot::Keep(i) => real[*i].text(src),
            Slot::Synth(tok) => canon_text(*tok),
        });
    }
    parts.join(" ")
}

/// Canonical lexable text for a synthesized token (alphabet only).
fn canon_text(tok: TokenKind) -> &'static str {
    match tok {
        TokenKind::Int => "0",
        TokenKind::Ident => "x",
        TokenKind::Plus => "+",
        TokenKind::RParen => ")",
        TokenKind::RBracket => "]",
        TokenKind::Colon => ":",
        _ => unreachable!("non-alphabet synth token"),
    }
}

/// Last-resort result when no repair exists within the cost bound: wrap
/// the whole input in an `Error` node. Guarantees `recover_parse` always
/// returns a tree.
fn fallback_result(src: &str) -> RepairResult {
    let tokens = tokenize(src);
    let real: Vec<_> = tokens.iter().filter(|t| t.kind != TokenKind::Eof).collect();
    let skipped: Vec<TokenKind> = real.iter().map(|t| t.kind).collect();
    let repairs: Vec<Repair> = (0..real.len()).map(|at| Repair::Delete { at }).collect();
    let cost = repairs.len() as Cost;
    let width = src.len() as u32;
    RepairResult { tree: Arc::new(Node::error(skipped, width)), repairs, cost }
}

/// A slot in a candidate token sequence: either an original token (by
/// index) kept in place, or a synthesized (inserted/substituted) token.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Slot {
    Keep(usize),
    Synth(TokenKind),
}

/// Dijkstra frontier item. Ordering is by `key` only (min-heap via
/// reversed `cmp`); `slots`/`repairs` are payload.
struct HeapItem {
    key: (Cost, usize, Vec<(u8, usize, u8)>),
    slots: Vec<Slot>,
    repairs: Vec<Repair>,
}

impl PartialEq for HeapItem {
    fn eq(&self, o: &Self) -> bool {
        self.key == o.key
    }
}
impl Eq for HeapItem {}
impl PartialOrd for HeapItem {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for HeapItem {
    fn cmp(&self, o: &Self) -> Ordering {
        // Reversed: BinaryHeap is a max-heap, we want smallest key first.
        o.key.cmp(&self.key)
    }
}

// ---------------------------------------------------------------------------
// M3: localized repair — confine the search to the precedence-bounded region
// the parser had open at the error, escalating outward when its boundary is
// not well-formed.
// ---------------------------------------------------------------------------

/// Outcome of a localized repair: the repair result, the token window it was
/// found in (`None` once the search escalated to the full stream), and
/// whether escalation reached the global scope.
#[derive(Debug, Clone)]
pub struct LocalRepairResult {
    pub result: RepairResult,
    pub region: Option<(usize, usize)>,
    pub escalated_to_global: bool,
}

/// Cost-optimal repair confined to the innermost well-formed precedence-bounded
/// region around the error, escalating outward when a region's boundary is the
/// error (e.g. a missing closer). The final fallback is the full stream, which
/// is exactly `global_optimal_repair`.
///
/// Theorem 4.2 (empirically validated in the tests and M4): when the region
/// boundary is well-formed, the cost found here equals the global optimum.
pub fn local_optimal_repair(src: &str) -> LocalRepairResult {
    local_optimal_repair_bounded(src, MAX_REPAIR_COST)
}

/// `local_optimal_repair` with an explicit cost bound (see
/// `global_optimal_repair_bounded`). The windowed region searches and the
/// global fallback all use `max_cost`, so the local-vs-global cost-equality
/// property holds at any bound.
pub fn local_optimal_repair_bounded(src: &str, max_cost: Cost) -> LocalRepairResult {
    let tokens = tokenize(src);
    let costs = RepairCosts::default();
    let real_end = tokens.len().saturating_sub(1); // index of the Eof token
    // Open contexts at the error, innermost last (approach (a): from the
    // parser's actual recursion, not a token re-scan).
    let ctxs = open_contexts_at_error(&tokens, src);

    // Try each context's precedence-bounded region, innermost outward. A
    // missing-closer (ill-formed) boundary yields no window, escalating to
    // the enclosing context (Theorem 3.8).
    for (j, ctx) in ctxs.iter().enumerate().rev() {
        if let Some(window) = region_window(ctx, &ctxs[..j], &tokens, real_end) {
            // The region is a valid Theorem-4.2 locus only if it is the sole
            // error (its placeholder skeleton parses); otherwise the optimal
            // repair may restructure tokens outside it, so escalate.
            if region_is_sole_error(&tokens, src, window) {
                if let Some(result) = optimal_repair(&tokens, src, Some(window), &costs, max_cost) {
                    return LocalRepairResult {
                        result,
                        region: Some(window),
                        escalated_to_global: false,
                    };
                }
            }
        }
    }

    // Escalated to the full stream: identical to the global oracle.
    let result = global_optimal_repair_bounded(src, max_cost);
    LocalRepairResult { result, region: None, escalated_to_global: true }
}

/// Run a fresh instrumented parse and return the contexts left open at the
/// first error (innermost last). Empty if the input parses cleanly or fails
/// with all contexts closed (e.g. trailing tokens at the top level).
fn open_contexts_at_error(tokens: &[Token], src: &str) -> Vec<Ctx> {
    let mut p = RecoveringParser { src, tokens, pos: 0, stack: Vec::new() };
    let _ = p.parse_expr(MIN_PREC);
    p.stack
}

/// The precedence-bounded region (editable token window) for a context, or
/// `None` if its boundary is the error (a group with a missing closer).
/// `outer` is the enclosing context chain (those below `ctx` on the stack).
fn region_window(
    ctx: &Ctx,
    outer: &[Ctx],
    tokens: &[Token],
    real_end: usize,
) -> Option<(usize, usize)> {
    match *ctx {
        Ctx::Group { opener, awaits } => {
            matching_closer(tokens, opener, awaits).map(|closer| (opener + 1, closer))
        }
        Ctx::Expr { min_prec, start_tok } => {
            // A frame nested inside an *unclosed* group does not have a
            // well-formed boundary: the cheapest repair may involve the
            // missing bracket, which lies outside the frame (e.g. `a[` is
            // fixed by deleting `[`, not by filling the index). Theorem 4.2
            // does not apply, so escalate rather than repair locally.
            // NOTE: soundness is guaranteed solely by `region_is_sole_error`
            // (the skeleton check) at the call site — verified empirically by
            // relaxing the two escalations below and finding the corpus + the
            // 2000-case proptest still pass. The escalations here are *tightness
            // and honesty optimizations*, not soundness conditions: they keep
            // the proposed window tight and report `escalated_to_global` (rather
            // than dressing a full-width window up as a "region") when the error
            // is not locally containable, and they avoid an oracle call on a
            // window the skeleton check would reject anyway.
            match nearest_enclosing_group(outer) {
                Some((opener, awaits)) => match matching_closer(tokens, opener, awaits) {
                    // Enclosing group intact: bound the valley by its closer.
                    Some(closer) => valley_boundary(tokens, start_tok, min_prec, closer)
                        .map(|v| (start_tok, v)),
                    // Enclosing group unclosed: not locally containable, escalate.
                    None => None,
                },
                // No enclosing group: bounded by EOF.
                None => {
                    valley_boundary(tokens, start_tok, min_prec, real_end).map(|v| (start_tok, v))
                }
            }
        }
    }
}

/// The nearest enclosing bracket group in the context chain `outer`
/// (innermost first), or `None` if the frame is at the top level.
fn nearest_enclosing_group(outer: &[Ctx]) -> Option<(usize, TokenKind)> {
    outer.iter().rev().find_map(|c| match *c {
        Ctx::Group { opener, awaits } => Some((opener, awaits)),
        Ctx::Expr { .. } => None,
    })
}

/// A region is the *sole* error locus — and thus a valid Theorem-4.2 reuse
/// region — iff replacing its interior with a single placeholder operand
/// yields a parseable skeleton. This rules out cases where the optimal repair
/// restructures tokens *outside* the region (a context-dependent `(` whose
/// grouping-vs-call role is fixed by the token before it; or a second error
/// elsewhere), where local repair can be globally suboptimal. It refines the
/// proof sketch's "boundary well-formed" hypothesis, which M4 showed to be
/// insufficient on its own (counterexample `(()`).
fn region_is_sole_error(tokens: &[Token], src: &str, window: (usize, usize)) -> bool {
    let real = &tokens[..tokens.len().saturating_sub(1)];
    let (s, e) = window;
    let mut slots: Vec<Slot> = (0..s).map(Slot::Keep).collect();
    slots.push(Slot::Synth(TokenKind::Int));
    slots.extend((e..real.len()).map(Slot::Keep));
    crate::parser::parse(&render(&slots, real, src)).is_ok()
}

/// Index of the closer matching the bracket opener at `opener` (balanced over
/// the same bracket kind), or `None` if no matching closer exists in the
/// stream — which means the boundary is the error.
fn matching_closer(tokens: &[Token], opener: usize, awaits: TokenKind) -> Option<usize> {
    let open_kind = tokens[opener].kind;
    let mut depth: i32 = 1;
    for (i, t) in tokens.iter().enumerate().skip(opener + 1) {
        if t.kind == open_kind {
            depth += 1;
        } else if t.kind == awaits {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// Right boundary of a `parse_expr(min_prec)` frame's precedence-bounded
/// region, scanned with operand/operator alternation so operator-position
/// tokens are identified without a full parse.
///
/// Returns `Some(v)` when the scan reaches a **well-formed** boundary — the
/// `hard_end` (an intact enclosing closer or EOF), or a genuine operator
/// (`lbp > 0`) at which an enclosing frame continues. Returns `None` at an
/// **ill-formed** boundary — a stray/unbalanced closer, or an `lbp = 0` token
/// (atom/comma/colon) in operator position — which means the optimal repair
/// may need to edit that token, so the region is not Theorem-4.2-valid and
/// the caller must escalate. The cost-equality test (M4) guards this
/// characterization against drift.
fn valley_boundary(
    tokens: &[Token],
    start_tok: usize,
    min_prec: u32,
    hard_end: usize,
) -> Option<usize> {
    let mut i = start_tok;
    let mut expect_operand = true;
    while i < hard_end {
        let k = tokens[i].kind;
        match k {
            TokenKind::LParen | TokenKind::LBracket => {
                i = skip_group(tokens, i, hard_end);
                expect_operand = false; // a group is an operand; expect operator next
                continue;
            }
            // A closer before hard_end is unbalanced/stray — ill-formed.
            TokenKind::RParen | TokenKind::RBracket => return None,
            _ => {}
        }
        if expect_operand {
            match k {
                TokenKind::Minus | TokenKind::Bang => i += 1, // prefix; still expecting operand
                TokenKind::Int | TokenKind::Ident => {
                    i += 1;
                    expect_operand = false;
                }
                // An infix operator in operand position is a real precedence
                // valley (insert an operand; the operator continues). A
                // non-operator (comma/colon) is an ill-formed boundary.
                _ => return if lbp(k) > 0 { Some(i) } else { None },
            }
        } else if k == TokenKind::Dot {
            i += 2; // postfix member `.field`; result is an operand → expect operator
        } else {
            let l = lbp(k);
            if l > min_prec {
                i += 1;
                expect_operand = true; // infix absorbed; expect operand
            } else if l > 0 {
                return Some(i); // genuine precedence valley
            } else {
                return None; // lbp 0 in operator position (atom/comma) — ill-formed
            }
        }
    }
    Some(hard_end) // reached an intact enclosing closer or EOF — well-formed
}

/// Balanced-skip from a `(`/`[` at `i` to just past its matching closer,
/// bounded by `hard_end`. Returns `hard_end` if unmatched.
fn skip_group(tokens: &[Token], i: usize, hard_end: usize) -> usize {
    let open = tokens[i].kind;
    let close = match open {
        TokenKind::LParen => TokenKind::RParen,
        TokenKind::LBracket => TokenKind::RBracket,
        _ => return i + 1,
    };
    let mut depth = 0i32;
    let mut j = i;
    while j < hard_end {
        let k = tokens[j].kind;
        if k == open {
            depth += 1;
        } else if k == close {
            depth -= 1;
            if depth == 0 {
                return j + 1;
            }
        }
        j += 1;
    }
    hard_end
}

/// An open parse context used to locate the precedence-bounded region of an
/// error: a `parse_expr` frame (precedence-valley tightening, M3b) or a
/// bracket group (bracket-scope region + missing-closer escalation, M3a).
/// Pushed on enter/open, popped on exit/close (success path only), so at an
/// error the stack holds the live region chain, innermost last.
#[derive(Debug, Clone, Copy)]
enum Ctx {
    Expr { min_prec: u32, start_tok: usize },
    Group { opener: usize, awaits: TokenKind },
}

/// A fresh Pratt parser instrumented to maintain the open-context stack via
/// the `PrattCore` recovery hooks. Used only to locate the error's enclosing
/// region; its parse output is discarded.
struct RecoveringParser<'a> {
    src: &'a str,
    tokens: &'a [Token],
    pos: usize,
    stack: Vec<Ctx>,
}

impl<'a> PrattCore<'a> for RecoveringParser<'a> {
    fn src(&self) -> &'a str {
        self.src
    }
    fn tokens(&self) -> &'a [Token] {
        self.tokens
    }
    fn pos(&self) -> usize {
        self.pos
    }
    fn set_pos(&mut self, pos: usize) {
        self.pos = pos;
    }
    fn on_enter_expr(&mut self, min_prec: u32, start_tok: usize) {
        self.stack.push(Ctx::Expr { min_prec, start_tok });
    }
    fn on_exit_expr(&mut self) {
        self.stack.pop();
    }
    fn on_open_group(&mut self, opener: usize, awaits: TokenKind) {
        self.stack.push(Ctx::Group { opener, awaits });
    }
    fn on_close_group(&mut self) {
        self.stack.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::NodeKind;
    use crate::parser::parse;

    /// Clean-input invariant: on every valid input, `recover_parse`
    /// returns exactly the fresh-parse tree and no repairs.
    #[test]
    fn clean_input_identical_to_fresh() {
        let valid = [
            "42",
            "a + b + c",
            "a + b * c",
            "a ^ b ^ c",
            "-a + b",
            "a ? b : c ? d : e",
            "(a + b) * c",
            "f()",
            "f(a, b, c)",
            "f(a + b * c)",
            "a.b.c",
            "a[i]",
            "a.b(x)[i].c",
            "x && y || z != 0",
        ];
        for s in valid {
            let (tree, repairs) = recover_parse(s);
            let fresh = parse(s).expect("valid input parses");
            assert_eq!(tree, fresh, "tree differs from fresh parse for {s:?}");
            assert!(repairs.is_empty(), "expected no repairs for valid {s:?}");
            // No recovery markers in a clean tree.
            assert!(!has_recovery_marker(&tree), "unexpected marker in {s:?}");
        }
    }

    /// Broken input never panics and always yields a tree (M1 fallback).
    #[test]
    fn broken_input_yields_a_tree_without_panicking() {
        let broken = ["1 +", "(1 + 2", "1 + * 2", "a ? b c", "f(a, , b)", ")", "1 2"];
        for s in broken {
            let (_tree, _repairs) = recover_parse(s); // must not panic
        }
    }

    /// M2: the optimal repair is complete (its repairs, applied, parse),
    /// cost-consistent (`cost == total(repairs)`), and of the known
    /// minimal cost. Exact structure asserted for the unambiguous cases.
    #[test]
    fn m2_optimal_repair_complete_minimal_consistent() {
        // (input, expected minimal cost, optional expected normalized tree)
        let cases: &[(&str, Cost, Option<&str>)] = &[
            ("1 +", 1, Some("(1 + 0)")),
            ("(1 + 2", 1, Some("(1 + 2)")),
            ("1 + * 2", 1, None),
            ("1 2", 1, Some("(1 + 2)")),
            ("a ? b c", 1, Some("(a ? b : c)")),
            (")", 1, Some("0")),
            ("f(a, , b)", 1, None),
            ("((1", 2, Some("1")),
            ("a ? b", 1, None),
            ("f(a", 1, None),
            ("a[i", 1, None),
        ];
        for (src, expected_cost, expected_tree) in cases {
            let r = global_optimal_repair(src);
            assert_eq!(
                r.cost,
                RepairCosts::default().total(&r.repairs),
                "cost/total mismatch for {src:?}: {:?}",
                r.repairs
            );
            assert_eq!(
                r.cost, *expected_cost,
                "unexpected min cost for {src:?}: repairs {:?}",
                r.repairs
            );
            // Independent completeness check: reconstruct the repaired
            // stream from the repair list alone and confirm it parses to
            // the same tree the search returned.
            let repaired = apply_repairs(src, &r.repairs);
            let reparsed = parse(&repaired).unwrap_or_else(|e| {
                panic!("repaired {src:?} -> {repaired:?} did not parse: {e:?}")
            });
            assert_eq!(
                reparsed.unparse_normalized(),
                r.tree.unparse_normalized(),
                "tree and repairs disagree for {src:?} (repaired {repaired:?})"
            );
            if let Some(expected) = expected_tree {
                assert_eq!(
                    r.tree.unparse_normalized(),
                    *expected,
                    "unexpected tree for {src:?}: repairs {:?}",
                    r.repairs
                );
            }
        }
    }

    /// M3 / Theorem 4.2 (empirical): localized repair finds the same
    /// minimum cost as the global oracle on every corpus input — both the
    /// well-formed-boundary cases (repaired locally) and the ill-formed
    /// ones (escalated to global).
    #[test]
    fn m3_local_cost_equals_global_cost() {
        let corpus = [
            "1 +",
            "(1 + 2",      // missing closer -> escalates
            "1 + * 2",
            "1 2",
            "a ? b c",
            ")",
            "f(a, , b)",
            "((1",          // missing closers -> escalates
            "(1 + )",       // local: interior of intact paren
            "f(1 +)",       // local: interior of intact call
            "a[1 +]",       // local: interior of intact index
            "( 1 * ) + 2",  // local repair, untouched tail
            "(((1+2)))",    // valid: cost 0
        ];
        for s in corpus {
            let local = local_optimal_repair(s);
            let global = global_optimal_repair(s);
            assert_eq!(
                local.result.cost, global.cost,
                "local≠global cost for {s:?}: local {:?} (region {:?}), global {:?}",
                local.result.repairs, local.region, global.repairs
            );
        }
    }

    /// M3 locality: an error inside an intact bracket scope is repaired
    /// within a window whose size is constant in the surrounding
    /// expression length — the repair does not re-search the growing tail.
    #[test]
    fn m3_local_window_is_constant_in_tail_length() {
        let mut regions = Vec::new();
        for n in [3usize, 10, 30] {
            let tail: String = (2..n).map(|k| format!(" + {k}")).collect();
            let src = format!("(1 + ){tail}"); // error inside the first paren
            let local = local_optimal_repair(&src);
            let global = global_optimal_repair(&src);
            assert_eq!(local.result.cost, global.cost, "cost mismatch n={n}");
            assert_eq!(local.result.cost, 1, "expected single insert n={n}");
            assert!(!local.escalated_to_global, "should repair locally n={n}");
            regions.push(local.region.expect("local region"));
        }
        assert!(
            regions.iter().all(|r| *r == regions[0]),
            "repair window must be constant in tail length, got {regions:?}"
        );
    }

    /// M3b: precedence-valley tightening localizes an error inside a *flat*
    /// (bracket-free) expression. The window is constant in the length of the
    /// surrounding flat chain — locality is a precedence property, not just
    /// bracket-matching.
    #[test]
    fn m3b_flat_expression_valley_is_constant() {
        let mut regions = Vec::new();
        for n in [4usize, 12, 40] {
            let tail: String = (0..n).map(|k| format!(" + v{k}")).collect();
            // "a * + v0 + v1 ..." — `*` is missing its right operand; the
            // error sits in a parse_expr(rbp(*)) frame whose valley is the
            // very next `+`, regardless of how long the chain runs.
            let src = format!("a *{tail}");
            let local = local_optimal_repair(&src);
            let global = global_optimal_repair(&src);
            assert_eq!(local.result.cost, global.cost, "cost mismatch n={n}");
            assert_eq!(local.result.cost, 1, "expected single insert n={n}");
            assert!(!local.escalated_to_global, "should repair locally (flat) n={n}");
            regions.push(local.region.expect("local region"));
        }
        assert!(
            regions.iter().all(|r| *r == regions[0]),
            "flat-expression repair window must be constant, got {regions:?}"
        );
    }

    /// Reconstruct the repaired source string from the original input and
    /// a repair list, independently of the search. Validates that the
    /// `repairs` field genuinely produces a parseable stream.
    fn apply_repairs(src: &str, repairs: &[Repair]) -> String {
        use std::collections::{HashMap, HashSet};
        let toks = tokenize(src);
        let real = &toks[..toks.len() - 1];
        let mut inserts: HashMap<usize, Vec<TokenKind>> = HashMap::new();
        let mut deletes: HashSet<usize> = HashSet::new();
        let mut subs: HashMap<usize, TokenKind> = HashMap::new();
        for r in repairs {
            match r {
                Repair::Insert { at, tok } => inserts.entry(*at).or_default().push(*tok),
                Repair::Delete { at } => {
                    deletes.insert(*at);
                }
                Repair::Substitute { at, to } => {
                    subs.insert(*at, *to);
                }
            }
        }
        let mut parts: Vec<String> = Vec::new();
        for idx in 0..=real.len() {
            if let Some(toks) = inserts.get(&idx) {
                for t in toks {
                    parts.push(canon_text(*t).to_string());
                }
            }
            if idx < real.len() && !deletes.contains(&idx) {
                let text = match subs.get(&idx) {
                    Some(to) => canon_text(*to).to_string(),
                    None => real[idx].text(src).to_string(),
                };
                parts.push(text);
            }
        }
        parts.join(" ")
    }

    fn has_recovery_marker(n: &Node) -> bool {
        match &n.kind {
            NodeKind::Missing | NodeKind::Error { .. } => true,
            NodeKind::Atom(_) => false,
            NodeKind::Prefix { child, .. } => has_recovery_marker(child),
            NodeKind::Binary { left, right, .. } => {
                has_recovery_marker(left) || has_recovery_marker(right)
            }
            NodeKind::Ternary { cond, then, else_ } => {
                has_recovery_marker(cond)
                    || has_recovery_marker(then)
                    || has_recovery_marker(else_)
            }
            NodeKind::Paren { inner } => has_recovery_marker(inner),
            NodeKind::Call { callee, args } => {
                has_recovery_marker(callee) || args.iter().any(|a| has_recovery_marker(a))
            }
            NodeKind::Index { array, index } => {
                has_recovery_marker(array) || has_recovery_marker(index)
            }
            NodeKind::Member { object, .. } => has_recovery_marker(object),
        }
    }
}
