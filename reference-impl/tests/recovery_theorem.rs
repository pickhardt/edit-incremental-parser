//! M4 / Theorem 4.2 (empirical): localized cost-optimal repair finds the
//! **same minimum cost** as the global oracle, across thousands of random
//! valid expressions corrupted by random small edits.
//!
//! Why cost-equality is the right invariant: the global oracle searches the
//! whole token stream, a superset of any region window, so
//! `global.cost ≤ local.cost` always. Theorem 4.2 says `local.cost ≤
//! global.cost` when the region boundary is well-formed; escalation makes
//! `local == global` otherwise. So `local.cost == global.cost` must hold on
//! every input — and any inequality is a real bug (region wrong, escalation
//! missing, or the theorem's boundary hypothesis violated).

use incremental_pratt_poc::recovery::{
    global_optimal_repair_bounded, local_optimal_repair_bounded,
};
use incremental_pratt_poc::Edit;

/// Cost bound for the oracle in tests. Single-edit corruptions are repaired
/// at cost 1–2; capping at 3 keeps the bounded search fast (it can't blow up
/// exploring deep frontiers on the rare unrepairable input) without affecting
/// the cost-equality property — both paths use the same bound.
const TEST_BOUND: u32 = 3;

mod proptests {
    use super::*;
    use proptest::prelude::*;

    /// Small valid expression (kept shallow so the global oracle's bounded
    /// Dijkstra stays cheap across many cases).
    fn arb_expr() -> impl Strategy<Value = String> {
        let atom = prop_oneof!["[a-c]", "[0-9]"];
        atom.prop_recursive(2, 8, 2, |inner| {
            prop_oneof![
                (
                    inner.clone(),
                    prop_oneof![Just("+"), Just("-"), Just("*"), Just("&&"), Just("==")],
                    inner.clone()
                )
                    .prop_map(|(l, op, r)| format!("{l} {op} {r}")),
                inner.clone().prop_map(|c| format!("- {c}")),
                inner.clone().prop_map(|c| format!("({c})")),
                inner.clone().prop_map(|c| format!("{c}(x)")),
                (inner.clone(), inner.clone()).prop_map(|(c, i)| format!("{c}[{i}]")),
            ]
        })
    }

    /// A single small *corrupting* edit, biased toward cheaply-repairable
    /// breaks (delete a byte; insert a stray operator or closer) so the
    /// bounded oracle terminates quickly. Most break the parse; the rest stay
    /// valid (cost 0 for both — still a valid equality check). Stray *openers*
    /// are deliberately omitted: they create unclosed groups whose optimal
    /// repair is multi-edit and dominates oracle runtime.
    fn arb_corruption(src: &str) -> impl Strategy<Value = Edit> {
        let len = src.len() as u32;
        let starts = 0u32..=len;
        let actions = prop_oneof![
            Just(("".to_string(), 1u32)),  // delete 1 byte
            Just(("".to_string(), 2u32)),  // delete 2 bytes
            Just(("+".to_string(), 0u32)), // insert stray +
            Just(("*".to_string(), 0u32)), // insert stray *
            Just((")".to_string(), 0u32)), // insert stray )
            Just(("]".to_string(), 0u32)), // insert stray ]
        ];
        (starts, actions).prop_map(move |(s, (repl, dl))| {
            let end = (s + dl).min(len);
            Edit { start: s, end, replacement: repl }
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 2000, .. ProptestConfig::default() })]

        #[test]
        fn local_cost_equals_global_cost(src in arb_expr(), seed in any::<[u8; 32]>()) {
            use proptest::strategy::ValueTree;
            use proptest::test_runner::{Config, TestRunner};
            let mut runner = TestRunner::new_with_rng(
                Config::default(),
                proptest::test_runner::TestRng::from_seed(
                    proptest::test_runner::RngAlgorithm::ChaCha,
                    &seed,
                ),
            );
            let edit = arb_corruption(&src).new_tree(&mut runner).unwrap().current();
            let broken = edit.apply(&src);

            // Keep the bounded oracle search cheap.
            prop_assume!(broken.len() <= 16);

            let local = local_optimal_repair_bounded(&broken, TEST_BOUND);
            let global = global_optimal_repair_bounded(&broken, TEST_BOUND);

            // Theorem 4.2 (empirical): the localized optimum equals the
            // global optimum.
            prop_assert_eq!(
                local.result.cost,
                global.cost,
                "local≠global on {:?}: local {:?} (region {:?}, escalated {}), global {:?}",
                broken,
                local.result.repairs,
                local.region,
                local.escalated_to_global,
                global.repairs
            );

            // Internal consistency: a region was used iff we did not escalate
            // all the way to the global scope.
            prop_assert_eq!(local.region.is_some(), !local.escalated_to_global);
        }
    }
}
