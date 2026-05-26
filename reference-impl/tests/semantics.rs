//! Identity-keyed incremental semantics — validation (Phase 4 of
//! `identity_keyed_semantics_plan.md`), for the Phase 2 (context-free) `eval`.
//!
//! Two claims:
//!   1. **Soundness:** incremental memoized `eval` over a reparsed tree equals
//!      a from-scratch `eval` of the new source, for arbitrary edits.
//!   2. **Locality:** for an edit localized in a large expression, the number
//!      of nodes recomputed is small (∝ the changed region + the balanced
//!      spine), not ∝ the whole tree.
//!
//! Run with: `cargo test --features node_id --test semantics`.
#![cfg(feature = "node_id")]

use incremental_pratt_poc::semantics::{eval_fresh, IncrementalEvaluator};
use incremental_pratt_poc::{incremental_parse, parse, Edit};

/// Parse `old_src`, prime an evaluator on it, apply `edit`, reparse
/// incrementally, and evaluate the new tree with the same (warm) evaluator.
/// Returns (incremental value, fresh value, nodes recomputed on the second
/// eval, total nodes in the new tree). Returns `None` if either source fails
/// to parse (the edit produced invalid syntax) — same skip policy as the
/// parse-equivalence harness.
fn eval_after_edit(old_src: &str, edit: &Edit) -> Option<(i64, i64, usize, u32)> {
    let new_src = edit.apply(old_src);
    let old_tree = parse(old_src).ok()?;
    let fresh_new = parse(&new_src).ok()?;
    let (incr_new, returned_new_src, _stats) =
        incremental_parse(&old_tree, old_src, edit).ok()?;
    assert_eq!(returned_new_src, new_src);

    let mut ev = IncrementalEvaluator::new();
    let _ = ev.eval(&old_tree); // prime memo with the old tree's node ids
    let incr_val = ev.eval(&incr_new); // warm: reused subtrees hit the memo
    let recomputed = ev.recomputed;

    let fresh_val = eval_fresh(&fresh_new);
    Some((incr_val, fresh_val, recomputed, incr_new.count()))
}

fn check_eval_equiv(old_src: &str, edit: Edit) {
    if let Some((incr, fresh, _, _)) = eval_after_edit(old_src, &edit) {
        assert_eq!(
            incr, fresh,
            "incremental eval {} != fresh eval {} on old=`{}` edit {}..{} -> `{}`",
            incr, fresh, old_src, edit.start, edit.end, edit.replacement
        );
    }
}

/// Stronger soundness check used to stress the chain splice: the incremental
/// tree must be **structurally identical** to a fresh parse (`unparse()`), and
/// the memoized eval must match the fresh value. (For in-place operand edits
/// the spliced grouping equals fresh `build_balanced`'s, so even strict
/// `unparse()` — which does NOT normalize associativity — must agree.)
fn check_structural_and_value_equiv(old_src: &str, edit: &Edit) {
    let new_src = edit.apply(old_src);
    let fresh = match parse(&new_src) {
        Ok(n) => n,
        Err(_) => return,
    };
    let old_tree = match parse(old_src) {
        Ok(n) => n,
        Err(_) => return,
    };
    let (incr, _new_src, _stats) =
        incremental_parse(&old_tree, old_src, edit).expect("incremental should parse");
    // Compare up to ≈ (associativity-conflict reassociation): the general
    // chain splice may regroup an assoc chain on insert/delete, which is sound
    // by Paper 2 §5. `unparse_normalized` flattens those chains, so a correct
    // regrouping agrees; value equivalence is the independent semantic check.
    assert_eq!(
        incr.unparse_normalized(),
        fresh.unparse_normalized(),
        "structural (≈) mismatch on old=`{}` edit {}..{} -> `{}`",
        old_src, edit.start, edit.end, edit.replacement
    );
    let mut ev = IncrementalEvaluator::new();
    let _ = ev.eval(&old_tree);
    assert_eq!(ev.eval(&incr), eval_fresh(&fresh), "value mismatch");
}

// --- Sanity: the interpreter itself ---

#[test]
fn eval_basic_arithmetic() {
    assert_eq!(eval_fresh(&parse("1 + 2 * 3").unwrap()), 7);
    assert_eq!(eval_fresh(&parse("(1 + 2) * 3").unwrap()), 9);
    assert_eq!(eval_fresh(&parse("2 ^ 10").unwrap()), 1024);
    assert_eq!(eval_fresh(&parse("- 5 + 3").unwrap()), -2);
    assert_eq!(eval_fresh(&parse("7 % 3").unwrap()), 1);
    assert_eq!(eval_fresh(&parse("10 / 0").unwrap()), 0); // total: div-by-zero -> 0
}

#[test]
fn eval_associative_regrouping_is_value_stable() {
    // `+` is an AssociativityConflict op: the balanced builder may regroup it.
    // Integer `+` is associative, so eval must be invariant to the grouping.
    let chain = "1 + 2 + 3 + 4 + 5 + 6 + 7 + 8";
    assert_eq!(eval_fresh(&parse(chain).unwrap()), 36);
}

// --- Soundness on the hand-picked edits from the parse-equivalence suite ---

#[test]
fn eval_equiv_touch_operand() {
    check_eval_equiv("1 + 2 * 3", Edit { start: 4, end: 5, replacement: "20".to_string() });
}

#[test]
fn eval_equiv_change_operator_precedence() {
    check_eval_equiv("1 + 2 * 3", Edit { start: 2, end: 3, replacement: "*".to_string() });
}

#[test]
fn eval_equiv_edit_inside_parens() {
    check_eval_equiv("(1 + 2) * 3", Edit { start: 4, end: 5, replacement: "-".to_string() });
}

#[test]
fn eval_equiv_insert_subexpression() {
    check_eval_equiv("1 + 9", Edit { start: 4, end: 4, replacement: "2 * ".to_string() });
}

// --- Locality: recompute scales with the edit, not the file ---

#[test]
fn recompute_is_local_when_siblings_are_reusable() {
    // Many independent sibling subexpressions as function-call arguments:
    // `f(a_1, a_2, ..., a_N)`, each `a_i = (i + i * 2)`. Editing inside ONE
    // argument reuses every other argument wholesale (each is a cached subtree
    // whose NodeId is preserved -> a single eval memo hit, its interior never
    // revisited). So recompute is ∝ the edited argument, not the file.
    let n = 100;
    let args: Vec<String> = (1..=n).map(|i| format!("({} + {} * 2)", i, i)).collect();
    let src = format!("f({})", args.join(", "));

    // Edit one operand inside the middle argument.
    let mid = n / 2;
    let needle = format!("({} + {} * 2)", mid, mid);
    let at = src.find(&needle).expect("argument present") as u32;
    let start = at + 1; // first digit of the argument, just past `(`
    let digits = mid.to_string();
    let edit = Edit {
        start,
        end: start + digits.len() as u32,
        replacement: "9999".to_string(),
    };

    let (incr, fresh, recomputed, total) =
        eval_after_edit(&src, &edit).expect("edit should reparse");

    assert_eq!(incr, fresh, "incremental eval diverged from fresh");

    // Each argument is ~6 nodes; total ~ 6n + callee + call ~ 600+. A
    // one-argument edit recomputes that argument's handful of nodes plus the
    // Call node and callee — an order of magnitude below the file.
    let bound = 32;
    assert!(
        recomputed <= bound,
        "recomputed {} nodes (total {}) for a one-argument edit; expected <= {} \
         (∝ edited argument, not file size)",
        recomputed, total, bound
    );
    assert!(
        (recomputed as u32) * 8 < total,
        "recompute {} should be a small fraction of total {}",
        recomputed, total
    );
}

#[test]
fn flat_chain_edit_locality() {
    // A long `+` chain of `(i * 7)` operands; edit one operand in place. The
    // recompute behavior depends on whether the parser does the O(log n) chain
    // splice (the `chain_splice` feature):
    //
    //   * WITHOUT chain_splice: `parse_assoc_chain` re-collects all operands and
    //     rebuilds the whole balanced interior, so every interior node gets a
    //     fresh id and eval recompute is O(n) — it faithfully tracks the
    //     parser's rebuild set (operands are reused; interior is rebuilt).
    //   * WITH chain_splice: the parser path-copies only the O(log n) ancestors
    //     to the edited operand and Arc-clones every off-path subtree, so eval
    //     recompute drops to O(log n) + the edited operand — independent of
    //     chain length.
    let n = 200;
    let operands: Vec<String> = (1..=n).map(|i| format!("({} * 7)", i)).collect();
    let src = operands.join(" + ");

    let mid = n / 2;
    let needle = format!("({} * 7)", mid);
    let at = src.find(&needle).expect("operand present") as u32;
    let start = at + 1;
    let edit = Edit {
        start,
        end: start + mid.to_string().len() as u32,
        replacement: "999".to_string(),
    };

    let old_tree = parse(&src).unwrap();
    let (incr_new, _new_src, _stats) = incremental_parse(&old_tree, &src, &edit).unwrap();

    let mut ev = IncrementalEvaluator::new();
    let _ = ev.eval(&old_tree);
    let incr_val = ev.eval(&incr_new);
    let fresh_val = eval_fresh(&parse(&edit.apply(&src)).unwrap());
    assert_eq!(incr_val, fresh_val, "incremental eval diverged from fresh");

    #[cfg(feature = "chain_splice")]
    {
        // O(log n): the balanced spine to the operand (~log2 n) plus the
        // operand's handful of nodes — a small constant multiple of log n,
        // independent of chain length. (log2(200) ≈ 8.)
        let total = incr_new.count() as usize;
        let bound = 48;
        assert!(
            ev.recomputed <= bound,
            "chain_splice: recompute {} should be O(log n) <= {} (total {})",
            ev.recomputed, bound, total
        );
    }
    #[cfg(not(feature = "chain_splice"))]
    {
        // O(n): operands reused, balanced interior rebuilt.
        assert!(
            ev.reused as u32 >= n as u32 / 2,
            "no splice: operands should be reused (eval hits {}, n={})",
            ev.reused, n
        );
        assert!(
            ev.recomputed >= n / 2 && ev.recomputed <= 3 * n,
            "no splice: recompute {} should track rebuilt interior ~n={}",
            ev.recomputed, n
        );
    }
}

#[cfg(feature = "chain_splice")]
#[test]
fn chain_insert_delete_is_local() {
    // Insert an operand into the middle of a long `+` chain. The general splice
    // keeps the O(log n) off-path subtrees and rebuilds only the balanced spine
    // over them, so eval recompute is O(log n) — not O(n).
    let n = 200;
    let operands: Vec<String> = (1..=n).map(|i| format!("({} * 7)", i)).collect();
    let src = operands.join(" + ");
    // Insert a new operand `(999 * 7) + ` before the middle operand.
    let mid = n / 2;
    let needle = format!("({} * 7)", mid);
    let at = src.find(&needle).expect("operand present") as u32;
    let edit = Edit { start: at, end: at, replacement: "(999 * 7) + ".to_string() };

    let old_tree = parse(&src).unwrap();
    let (incr_new, _new_src, _stats) = incremental_parse(&old_tree, &src, &edit).unwrap();
    let fresh = parse(&edit.apply(&src)).unwrap();

    let mut ev = IncrementalEvaluator::new();
    let _ = ev.eval(&old_tree);
    let incr_val = ev.eval(&incr_new);
    assert_eq!(incr_val, eval_fresh(&fresh), "insert: value mismatch");
    assert_eq!(
        incr_new.unparse_normalized(),
        fresh.unparse_normalized(),
        "insert: ≈-structural mismatch"
    );

    let total = incr_new.count() as usize;
    let bound = 64; // O(log n) spine + reparsed inserted operand
    assert!(
        ev.recomputed <= bound,
        "insert: recompute {} should be O(log n) <= {} (total {})",
        ev.recomputed, bound, total
    );
}

#[cfg(feature = "chain_splice")]
fn depth(n: &std::sync::Arc<incremental_pratt_poc::Node>) -> u32 {
    use incremental_pratt_poc::ast::NodeKind;
    1 + match &n.kind {
        NodeKind::Atom(_) | NodeKind::Missing | NodeKind::Error { .. } => 0,
        NodeKind::Prefix { child, .. } => depth(child),
        NodeKind::Binary { left, right, .. } => depth(left).max(depth(right)),
        NodeKind::Ternary { cond, then, else_ } => depth(cond).max(depth(then)).max(depth(else_)),
        NodeKind::Paren { inner } => depth(inner),
        NodeKind::Call { callee, args } => {
            depth(callee).max(args.iter().map(depth).max().unwrap_or(0))
        }
        NodeKind::Index { array, index } => depth(array).max(depth(index)),
        NodeKind::Member { object, .. } => depth(object),
    }
}

/// Sustained O(log n) depth across MANY insert/delete edits, via the
/// weight-balanced `join` (`chain_wb.rs`). Earlier split-and-rebuild-spine
/// approaches let imbalance accumulate inside reused units (depth grew
/// ~log(edits): 9 → 15 → 17 → 19 over 40/200/1000 on a 64-operand chain). The
/// `join` instead *merges* unit spines with rotations, maintaining the
/// weight-balance invariant globally, so depth plateaus at the WB constant
/// (~2.4·log₂ n, ~18 here) and is stable across unbounded edits. This test
/// asserts the WB invariant on every edit (the correctness proof) plus a depth
/// bound; a regression reintroducing growth fails both.
#[cfg(feature = "chain_splice")]
#[test]
fn chain_splice_depth_across_many_edits() {
    let n0 = 64;
    let mut src: String = (1..=n0)
        .map(|i| format!("({} + 0)", i))
        .collect::<Vec<_>>()
        .join(" + ");

    let base_depth = depth(&parse(&src).unwrap());
    let mut max_depth = base_depth;

    // 300 alternating insert/delete edits at an interior boundary.
    for k in 0..300 {
        let operands: Vec<&str> = src.split(" + ").collect();
        let i = 1 + (k % (operands.len().saturating_sub(2)).max(1));
        let mut off = 0usize;
        for o in operands.iter().take(i) {
            off += o.len() + 3; // operand + " + "
        }
        let edit = if k % 2 == 0 {
            Edit { start: off as u32, end: off as u32, replacement: "(7 + 0) + ".to_string() }
        } else {
            // delete operand i with its preceding " + "
            Edit {
                start: (off - 3) as u32,
                end: (off + operands[i].len()) as u32,
                replacement: String::new(),
            }
        };
        let old_tree = parse(&src).unwrap();
        let new_src = edit.apply(&src);
        if parse(&new_src).is_err() {
            continue;
        }
        let (incr, _ns, _st) = incremental_parse(&old_tree, &src, &edit).unwrap();
        // soundness every step
        assert_eq!(
            incr.unparse_normalized(),
            parse(&new_src).unwrap().unparse_normalized(),
            "divergence at edit {}",
            k
        );
        // The real correctness proof: every `+` spine node stays
        // weight-balanced (children operand-counts within DELTA=3). If the
        // weight-balanced join were buggy this fails regardless of depth.
        assert!(
            wb_balanced(&incr),
            "weight-balance invariant violated at edit {}",
            k
        );
        max_depth = max_depth.max(depth(&incr));
        src = new_src;
    }

    eprintln!("WBT-DEPTH base={} max={} over 300 edits", base_depth, max_depth);
    // The weight-balanced splice (chain_wb::join_all) merges unit spines with
    // rotations, maintaining the WB invariant, so depth is bounded by the
    // weight-balanced constant (~2.4·log2 n for DELTA=3) and STABLE across
    // unbounded edits — it plateaus (measured ~18 for a 64-operand chain at
    // 40/200/1000 edits) rather than growing with the edit count as the earlier
    // split-rebuild did. That is sustained O(log n) (with the standard WB
    // constant, not optimal constant 1). Bound: base + a generous slack that a
    // regression reintroducing growth would exceed.
    assert!(
        max_depth <= base_depth + 12,
        "WBT splice depth exceeded the weight-balanced bound: base {} -> max {}",
        base_depth, max_depth
    );
}

/// True iff every `+` spine node in the tree is weight-balanced: its two
/// children's chain-operand counts are within DELTA = 3. (Operands like
/// `(i + 0)` contain a trivial 2-operand `+` chain, also checked.)
#[cfg(feature = "chain_splice")]
fn wb_balanced(n: &std::sync::Arc<incremental_pratt_poc::Node>) -> bool {
    use incremental_pratt_poc::ast::NodeKind;
    use incremental_pratt_poc::TokenKind;
    let csize = |m: &std::sync::Arc<incremental_pratt_poc::Node>| -> u32 {
        match &m.kind {
            NodeKind::Binary { op: TokenKind::Plus, .. } => m.wb,
            _ => 1,
        }
    };
    match &n.kind {
        NodeKind::Binary { op: TokenKind::Plus, left, right } => {
            let (a, b) = (csize(left), csize(right));
            a <= 3 * b && b <= 3 * a && wb_balanced(left) && wb_balanced(right)
        }
        NodeKind::Binary { left, right, .. } => wb_balanced(left) && wb_balanced(right),
        NodeKind::Prefix { child, .. } | NodeKind::Paren { inner: child } => wb_balanced(child),
        NodeKind::Ternary { cond, then, else_ } => {
            wb_balanced(cond) && wb_balanced(then) && wb_balanced(else_)
        }
        NodeKind::Index { array, index } => wb_balanced(array) && wb_balanced(index),
        NodeKind::Member { object, .. } => wb_balanced(object),
        NodeKind::Call { callee, args } => {
            wb_balanced(callee) && args.iter().all(wb_balanced)
        }
        _ => true,
    }
}

#[test]
fn memo_stays_bounded_across_many_edits() {
    // Without GC the memo accumulates every node ever evaluated. Edit a large
    // chain many times and confirm the memo stays bounded (≈ live size) while
    // eval remains correct. (~400 operands ⇒ ~2000 nodes ⇒ over the GC floor.)
    let n = 400;
    let src0: String = (1..=n)
        .map(|i| format!("({} + 0)", i))
        .collect::<Vec<_>>()
        .join(" + ");
    let mut src = src0.clone();

    let mut ev = IncrementalEvaluator::new();
    let _ = ev.eval(&parse(&src).unwrap());
    let live = parse(&src).unwrap().count() as usize;

    let mut max_memo = ev.memo_len();
    for k in 0..50 {
        // In-place edit of one operand's multiplicand near the middle.
        let mid = n / 2 + (k % 5);
        let needle = format!("({} + 0)", mid);
        let Some(at) = src.find(&needle) else { continue };
        let start = (at + 1) as u32;
        let edit = Edit {
            start,
            end: start + mid.to_string().len() as u32,
            replacement: format!("{}", 1000 + k),
        };
        let new_src = edit.apply(&src);
        let old_tree = parse(&src).unwrap();
        let (incr, _ns, _st) = incremental_parse(&old_tree, &src, &edit).unwrap();
        let v = ev.eval(&incr);
        assert_eq!(v, eval_fresh(&parse(&new_src).unwrap()), "memo eval diverged at {}", k);
        max_memo = max_memo.max(ev.memo_len());
        src = new_src;
    }

    // GC keeps the memo within a small multiple of the live tree, not ∝ edits.
    // Without GC it would be ~live + 50·(fresh per edit) ≫ this bound.
    assert!(
        max_memo <= 8 * live,
        "memo not bounded: max {} vs live {} ({}× — GC should cap it)",
        max_memo, live, max_memo / live.max(1)
    );
}

// --- Property-based soundness over random integer-arithmetic edits ---

mod proptests {
    use super::*;
    use proptest::prelude::*;

    /// Generate a small valid integer-arithmetic expression (no identifiers,
    /// so `eval` is context-free / well-defined for Phase 2).
    fn arb_int_expr() -> impl Strategy<Value = String> {
        let atom = prop_oneof!["[1-9]", "[1-9][0-9]"];
        atom.prop_recursive(4, 48, 6, |inner| {
            prop_oneof![
                (
                    inner.clone(),
                    prop_oneof![
                        Just("+"),
                        Just("-"),
                        Just("*"),
                        Just("/"),
                        Just("%"),
                        Just("^"),
                    ],
                    inner.clone()
                )
                    .prop_map(|(l, op, r)| format!("{} {} {}", l, op, r)),
                inner.clone().prop_map(|c| format!("- {}", c)),
                inner.clone().prop_map(|c| format!("({})", c)),
            ]
        })
    }

    /// Arbitrary small edit (digits/operators/parens) on `src`.
    fn arb_edit(src: &str) -> impl Strategy<Value = Edit> {
        let len = src.len() as u32;
        let starts = 0u32..=len;
        let lens = 0u32..=len.min(6);
        let replacements = prop_oneof![
            Just("".to_string()),
            Just("7".to_string()),
            Just("42".to_string()),
            Just("+".to_string()),
            Just("*".to_string()),
            Just("(3)".to_string()),
            Just(" + 8 ".to_string()),
        ];
        (starts, lens, replacements).prop_map(move |(s, l, r)| {
            let end = (s + l).min(len);
            Edit { start: s, end, replacement: r }
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 3000, .. ProptestConfig::default() })]

        /// Stress the chain splice on LONG associative chains (the generic
        /// generator above only produces short ones). Build a chain of K
        /// operands `(v + 0)` joined by `+` or `*`, then edit the `v` inside
        /// one operand in place — exactly the case the splice fast-path
        /// handles. Assert structural + value equivalence with a fresh parse.
        #[test]
        fn deep_chain_in_operand_edit_is_sound(
            join in prop_oneof![Just(" + "), Just(" * ")],
            vals in prop::collection::vec(1u32..1000u32, 3..40),
            idx in any::<usize>(),
            newv in 1u32..100000u32,
        ) {
            let operands: Vec<String> = vals.iter().map(|v| format!("({} + 0)", v)).collect();
            let src = operands.join(join);
            let i = idx % operands.len();
            let mut off = 0usize;
            for o in operands.iter().take(i) {
                off += o.len() + join.len();
            }
            let vstart = off + 1; // just past `(`
            let vlen = vals[i].to_string().len();
            let edit = Edit {
                start: vstart as u32,
                end: (vstart + vlen) as u32,
                replacement: newv.to_string(),
            };
            check_structural_and_value_equiv(&src, &edit);
        }

        /// Stress the GENERAL splice (operand insert/delete) on long chains.
        /// Build a chain of K operands `(v + 0)`, then either insert a new
        /// operand or delete an existing one at a random interior position.
        /// Assert ≈-structural + value equivalence with a fresh parse.
        #[test]
        fn deep_chain_insert_delete_is_sound(
            join in prop_oneof![Just(" + "), Just(" * ")],
            vals in prop::collection::vec(1u32..1000u32, 4..40),
            idx in any::<usize>(),
            do_insert in any::<bool>(),
        ) {
            let operands: Vec<String> = vals.iter().map(|v| format!("({} + 0)", v)).collect();
            let src = operands.join(join);
            // Byte offset of the START of operand i (interior: 1..len-1).
            let i = 1 + (idx % (operands.len() - 2)); // keep it interior
            let mut off = 0usize;
            for o in operands.iter().take(i) {
                off += o.len() + join.len();
            }
            let edit = if do_insert {
                // Insert a fresh operand + joiner before operand i.
                Edit {
                    start: off as u32,
                    end: off as u32,
                    replacement: format!("(7 + 0){}", join),
                }
            } else {
                // Delete operand i together with its preceding joiner.
                Edit {
                    start: (off - join.len()) as u32,
                    end: (off + operands[i].len()) as u32,
                    replacement: String::new(),
                }
            };
            check_structural_and_value_equiv(&src, &edit);
        }

        #[test]
        fn incremental_eval_matches_fresh(src in arb_int_expr(), seed in any::<[u8; 32]>()) {
            use proptest::strategy::ValueTree;
            use proptest::test_runner::{Config, TestRunner};
            let mut runner = TestRunner::new_with_rng(
                Config::default(),
                proptest::test_runner::TestRng::from_seed(
                    proptest::test_runner::RngAlgorithm::ChaCha,
                    &seed,
                ),
            );
            let edit = arb_edit(&src).new_tree(&mut runner).unwrap().current();
            check_eval_equiv(&src, edit);
        }
    }
}
