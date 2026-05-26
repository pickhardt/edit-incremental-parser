//! Identity-keyed incremental semantics, Phase 3: the **context-dependent**
//! case (`identity_keyed_semantics_plan.md`).
//!
//! Phase 2 (`semantics.rs`) evaluated a context-free interpretation, where a
//! node's value depends only on its subtree. Real semantic facts — a name's
//! type, what an identifier resolves to — depend on *context* (the surrounding
//! bindings), and that is where node identity alone stops being enough.
//!
//! We model context as an environment `Env: name → value` and interpret
//! `Atom` identifiers as variable references. The load-bearing point:
//!
//!   * **Node identity is the *syntactic* leaf.** A subtree the parser reused
//!     keeps its `NodeId`, so its *syntax* is unchanged — but its *value* can
//!     still change if a binding it reads changed. Memoizing on `NodeId` alone
//!     is therefore **unsound** under a binding change
//!     (`NaiveNodeIdEvaluator` demonstrates the divergence).
//!   * **Context must be a tracked input.** The sound evaluator records, per
//!     node, the free-variable bindings its value depended on, and reuses a
//!     memo entry only when those bindings are unchanged (`ContextEvaluator`).
//!     Editing a binding then invalidates exactly the uses that read it — even
//!     though those use-nodes are syntactically unchanged and keep the same
//!     `NodeId` — while nodes that don't read it are reused. This is the
//!     salsa/Adapton discipline (companion paper §5.4, §8.2), bottoming out at
//!     the sound, identity-stable parse.
//!
//! Requires the `node_id` feature.

use std::collections::BTreeMap;
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ast::{Node, NodeId, NodeKind};
use crate::lexer::TokenKind;

/// Don't GC memos smaller than this (see `ContextEvaluator::eval`).
const GC_FLOOR: usize = 1024;

/// Variable environment (the "context"). `BTreeMap` for deterministic order.
pub type Env = BTreeMap<String, i64>;

/// The free-variable bindings a value depended on: `(name, value seen)` pairs.
/// A memo entry is valid only while the env still maps each `name` to `value`.
type Deps = Vec<(String, i64)>;

/// Evaluate `Atom` text: an integer literal, or a variable looked up in `env`
/// (unbound → 0). Returns the value and, if it was a variable read, that dep.
fn atom_value(s: &str, env: &Env) -> (i64, Deps) {
    if let Ok(n) = s.parse::<i64>() {
        (n, Vec::new())
    } else {
        let v = env.get(s).copied().unwrap_or(0);
        (v, vec![(s.to_string(), v)])
    }
}

/// Combine an operator with already-evaluated child values, in parse order.
/// Same arithmetic as `semantics::combine`, kept independent here.
fn combine(kind: &NodeKind, vals: &[i64]) -> i64 {
    let g = |i: usize| vals.get(i).copied().unwrap_or(0);
    match kind {
        NodeKind::Prefix { op, .. } => match op {
            TokenKind::Minus => g(0).wrapping_neg(),
            TokenKind::Bang => (g(0) == 0) as i64,
            _ => g(0),
        },
        NodeKind::Binary { op, .. } => {
            let (a, b) = (g(0), g(1));
            match op {
                TokenKind::Plus => a.wrapping_add(b),
                TokenKind::Minus => a.wrapping_sub(b),
                TokenKind::Star => a.wrapping_mul(b),
                TokenKind::Slash => a.checked_div(b).unwrap_or(0),
                TokenKind::Percent => a.checked_rem(b).unwrap_or(0),
                TokenKind::Caret => ipow(a, b),
                TokenKind::AndAnd => ((a != 0) && (b != 0)) as i64,
                TokenKind::OrOr => ((a != 0) || (b != 0)) as i64,
                TokenKind::EqEq => (a == b) as i64,
                TokenKind::BangEq => (a != b) as i64,
                TokenKind::Lt => (a < b) as i64,
                TokenKind::Gt => (a > b) as i64,
                TokenKind::LtEq => (a <= b) as i64,
                TokenKind::GtEq => (a >= b) as i64,
                _ => 0,
            }
        }
        NodeKind::Ternary { .. } => {
            if g(0) != 0 {
                g(1)
            } else {
                g(2)
            }
        }
        NodeKind::Paren { .. } => g(0),
        NodeKind::Call { .. } | NodeKind::Member { .. } => g(0),
        NodeKind::Index { .. } => g(0).wrapping_add(g(1)),
        NodeKind::Atom(_) | NodeKind::Missing | NodeKind::Error { .. } => 0,
    }
}

fn ipow(a: i64, b: i64) -> i64 {
    if b < 0 {
        return 0;
    }
    let (mut r, mut base, mut e) = (1i64, a, b as u64);
    while e > 0 {
        if e & 1 == 1 {
            r = r.wrapping_mul(base);
        }
        base = base.wrapping_mul(base);
        e >>= 1;
    }
    r
}

/// Merge child dependency lists (dedup by name; a var read in two places is one
/// dependency — the values agree, coming from the same env).
fn merge_deps(into: &mut Deps, from: &Deps) {
    for (k, v) in from {
        if !into.iter().any(|(k2, _)| k2 == k) {
            into.push((k.clone(), *v));
        }
    }
}

/// A dependency list still holds iff every recorded `(name, value)` matches the
/// current env (unbound counts as 0, matching `atom_value`).
fn deps_hold(deps: &Deps, env: &Env) -> bool {
    deps.iter()
        .all(|(k, v)| env.get(k).copied().unwrap_or(0) == *v)
}

/// Collect every node id reachable from `root` (full O(n) walk), for the GC.
fn collect_reachable(node: &Arc<Node>, out: &mut FxHashSet<NodeId>) {
    out.insert(node.id);
    for_each_child(&node.kind, |c| collect_reachable(c, out));
}

/// Invoke `f` on each child in parse order.
fn for_each_child<'a>(kind: &'a NodeKind, mut f: impl FnMut(&'a Arc<Node>)) {
    match kind {
        NodeKind::Atom(_) | NodeKind::Missing | NodeKind::Error { .. } => {}
        NodeKind::Prefix { child, .. } => f(child),
        NodeKind::Binary { left, right, .. } => {
            f(left);
            f(right);
        }
        NodeKind::Ternary { cond, then, else_ } => {
            f(cond);
            f(then);
            f(else_);
        }
        NodeKind::Paren { inner } => f(inner),
        NodeKind::Call { callee, args } => {
            f(callee);
            for a in args {
                f(a);
            }
        }
        NodeKind::Index { array, index } => {
            f(array);
            f(index);
        }
        NodeKind::Member { object, .. } => f(object),
    }
}

/// **Negative result.** Memoizes purely on `NodeId`, ignoring context. Sound
/// for context-free edits, but **unsound** when a binding changes: a reused
/// `NodeId` returns the value computed under the old env. Used in tests to
/// exhibit the divergence — not how you would build a real evaluator.
#[derive(Default)]
pub struct NaiveNodeIdEvaluator {
    memo: FxHashMap<NodeId, i64>,
}

impl NaiveNodeIdEvaluator {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn eval(&mut self, node: &Arc<Node>, env: &Env) -> i64 {
        if let Some(&v) = self.memo.get(&node.id) {
            return v; // BUG BY DESIGN: ignores whether `env` changed.
        }
        let v = if let NodeKind::Atom(s) = &node.kind {
            atom_value(s, env).0
        } else {
            let mut vals = Vec::new();
            for_each_child(&node.kind, |c| vals.push(self.eval(c, env)));
            combine(&node.kind, &vals)
        };
        self.memo.insert(node.id, v);
        v
    }
}

/// **Positive result.** Memoizes on `NodeId` *and* the free-variable bindings
/// the value read. A memo entry is reused only when every dependency still
/// holds in the current env; otherwise the node is recomputed. So a binding
/// change invalidates exactly the uses that read it (even though their
/// `NodeId`s are unchanged), and a source edit reuses any unchanged subtree
/// whose dependencies are unchanged.
#[derive(Default)]
pub struct ContextEvaluator {
    memo: FxHashMap<NodeId, (i64, Deps)>,
    /// Reachability-GC trigger (see `eval`).
    gc_threshold: usize,
    /// Nodes (re)computed on the most recent `eval` — fresh work.
    pub recomputed: usize,
    /// Memo hits on the most recent `eval` (dependencies verified unchanged).
    pub reused: usize,
}

impl ContextEvaluator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Evaluate `node` under `env`, reporting `recomputed`/`reused` for the call.
    /// Memo memory is bounded by the same reachability GC as
    /// `IncrementalEvaluator::eval`: retain only entries reachable from the
    /// current `node` once the memo outgrows its threshold. Perf-neutral (live
    /// entries are kept) and O(1) per edit but for the rare GC walk.
    pub fn eval(&mut self, node: &Arc<Node>, env: &Env) -> i64 {
        self.recomputed = 0;
        self.reused = 0;
        let v = self.eval_inner(node, env).0;
        if self.memo.len() > self.gc_threshold.max(GC_FLOOR) {
            let mut live = FxHashSet::default();
            collect_reachable(node, &mut live);
            self.memo.retain(|k, _| live.contains(k));
            self.gc_threshold = self.memo.len() * 2;
        }
        v
    }

    fn eval_inner(&mut self, node: &Arc<Node>, env: &Env) -> (i64, Deps) {
        if let Some((v, deps)) = self.memo.get(&node.id) {
            if deps_hold(deps, env) {
                self.reused += 1;
                return (*v, deps.clone());
            }
        }
        self.recomputed += 1;
        let (v, deps) = if let NodeKind::Atom(s) = &node.kind {
            atom_value(s, env)
        } else {
            let mut child_vals = Vec::new();
            let mut deps: Deps = Vec::new();
            for_each_child(&node.kind, |c| {
                let (cv, cd) = self.eval_inner(c, env);
                child_vals.push(cv);
                merge_deps(&mut deps, &cd);
            });
            (combine(&node.kind, &child_vals), deps)
        };
        self.memo.insert(node.id, (v, deps.clone()));
        (v, deps)
    }

    pub fn memo_len(&self) -> usize {
        self.memo.len()
    }
}

/// Reference (non-memoizing) context evaluation — the from-scratch oracle.
pub fn eval_ctx_fresh(node: &Arc<Node>, env: &Env) -> i64 {
    ContextEvaluator::new().eval(node, env)
}
