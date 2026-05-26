//! Phase 3 validation: context-dependent identity-keyed semantics.
//!
//! Demonstrates the load-bearing point of `identity_keyed_semantics_plan.md`
//! Phase 3: node identity is the *syntactic* leaf, but a value that depends on
//! a binding must track that binding as an explicit input.
//!
//! Run with: `cargo test --features node_id --test semantics_ctx`.
#![cfg(feature = "node_id")]

use incremental_pratt_poc::semantics_ctx::{
    eval_ctx_fresh, ContextEvaluator, Env, NaiveNodeIdEvaluator,
};
use incremental_pratt_poc::{incremental_parse, parse, Edit};

fn env(pairs: &[(&str, i64)]) -> Env {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

#[test]
fn ctx_eval_basic() {
    let t = parse("a + 1").unwrap();
    assert_eq!(eval_ctx_fresh(&t, &env(&[("a", 5)])), 6);
    assert_eq!(eval_ctx_fresh(&t, &env(&[("a", 41)])), 42);
}

/// THE NEGATIVE RESULT. Memoizing on NodeId alone is unsound when a binding
/// changes: the syntactically-unchanged tree (same NodeIds) returns the value
/// computed under the old environment. This is exactly why node identity is
/// necessary but NOT sufficient for context-dependent semantics.
#[test]
fn naive_nodeid_memo_is_unsound_under_binding_change() {
    let t = parse("a + 1").unwrap();

    let mut naive = NaiveNodeIdEvaluator::new();
    let env1 = env(&[("a", 5)]);
    assert_eq!(naive.eval(&t, &env1), 6); // primes the NodeId memo with a = 5

    // The binding changes; the *tree* (and its NodeIds) is identical.
    let env2 = env(&[("a", 10)]);
    let naive_again = naive.eval(&t, &env2); // returns the STALE memoized value
    let truth = eval_ctx_fresh(&t, &env2);

    assert_eq!(truth, 11, "fresh must reflect the new binding");
    assert_eq!(naive_again, 6, "naive NodeId memo returns the stale value");
    assert_ne!(
        naive_again, truth,
        "NodeId-only memoization is unsound under a binding change"
    );

    // THE POSITIVE RESULT: the context-tracking evaluator is correct here.
    let mut ctx = ContextEvaluator::new();
    let _ = ctx.eval(&t, &env1);
    assert_eq!(ctx.eval(&t, &env2), truth, "context evaluator stays sound");
}

/// THE POSITIVE RESULT, localized. A binding change invalidates exactly the
/// uses that read it; subtrees that don't read it are reused — even though all
/// NodeIds are unchanged (no edit happened, only the context changed).
#[test]
fn binding_change_invalidates_only_dependent_uses() {
    // Left subtree reads only `a`; right subtree reads only `b`.
    let t = parse("(a + a) * (b + b)").unwrap();
    let mut ctx = ContextEvaluator::new();

    let e1 = env(&[("a", 1), ("b", 1)]);
    assert_eq!(ctx.eval(&t, &e1), 4); // (1+1)*(1+1)

    // Change only `a`. Re-evaluate the SAME tree (identical NodeIds).
    let e2 = env(&[("a", 3), ("b", 1)]);
    let v = ctx.eval(&t, &e2);
    assert_eq!(v, eval_ctx_fresh(&t, &e2), "context evaluator sound");
    assert_eq!(v, 12, "(3+3)*(1+1)");

    // The `b`-only subtree must have been reused (its dependency {b:1} still
    // holds), so at least one memo hit occurred despite zero edits.
    assert!(
        ctx.reused >= 1,
        "the b-only subtree should be reused on an a-only binding change \
         (reused={}, recomputed={})",
        ctx.reused, ctx.recomputed
    );
}

/// A SOURCE edit (not a binding change): the context evaluator reuses unchanged
/// subtrees (NodeId preserved by the incremental parser AND dependencies
/// unchanged) and recomputes the edited region — sound vs a fresh parse.
#[test]
fn source_edit_reuses_unchanged_under_context() {
    let src = "(a + a) * (b + b)";
    let environment = env(&[("a", 2), ("b", 5)]);

    let old_tree = parse(src).unwrap();
    let mut ctx = ContextEvaluator::new();
    let _ = ctx.eval(&old_tree, &environment); // prime

    // Edit the left subtree: `a + a` -> `a + 7`.
    let edit = Edit { start: 5, end: 6, replacement: "7".to_string() };
    let new_src = edit.apply(src);
    let fresh = parse(&new_src).unwrap();
    let (incr, _ns, _stats) = incremental_parse(&old_tree, src, &edit).unwrap();

    let v = ctx.eval(&incr, &environment);
    assert_eq!(v, eval_ctx_fresh(&fresh, &environment), "sound vs fresh");
    // The untouched `(b + b)` subtree is reused (same NodeIds, deps unchanged).
    assert!(
        ctx.reused >= 1,
        "the unedited right subtree should be reused (reused={}, recomputed={})",
        ctx.reused, ctx.recomputed
    );
}

mod proptests {
    use super::*;
    use proptest::prelude::*;

    /// Expressions over the variables a..d and small ints.
    fn arb_var_expr() -> impl Strategy<Value = String> {
        let atom = prop_oneof!["[a-d]", "[1-9]"];
        atom.prop_recursive(4, 40, 6, |inner| {
            prop_oneof![
                (inner.clone(), prop_oneof![Just("+"), Just("-"), Just("*")], inner.clone())
                    .prop_map(|(l, op, r)| format!("{} {} {}", l, op, r)),
                inner.clone().prop_map(|c| format!("({})", c)),
                inner.prop_map(|c| format!("- {}", c)),
            ]
        })
    }

    fn arb_env() -> impl Strategy<Value = Env> {
        (0i64..7, 0i64..7, 0i64..7, 0i64..7).prop_map(|(a, b, c, d)| {
            env(&[("a", a), ("b", b), ("c", c), ("d", d)])
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 3000, .. ProptestConfig::default() })]

        /// Soundness under a BINDING change: priming the context evaluator with
        /// env1 then evaluating the same tree under env2 must equal a fresh
        /// evaluation under env2 — for any expression and any two environments.
        #[test]
        fn ctx_eval_sound_under_binding_change(
            src in arb_var_expr(),
            e1 in arb_env(),
            e2 in arb_env(),
        ) {
            let t = match parse(&src) { Ok(n) => n, Err(_) => return Ok(()) };
            let mut ctx = ContextEvaluator::new();
            let _ = ctx.eval(&t, &e1);
            prop_assert_eq!(ctx.eval(&t, &e2), eval_ctx_fresh(&t, &e2));
        }
    }
}
