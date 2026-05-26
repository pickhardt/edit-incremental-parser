//! M5: recovery *inside* the incremental parser. A clean edit takes the fast
//! reuse path (no repairs, result equals a fresh parse); an edit that
//! introduces a syntax error is recovered via localized cost-optimal repair,
//! and the recovery is globally cost-optimal (Theorem 4.2 in the incremental
//! setting). This is the gap Diekmann [2019 §3.1] names as unfilled.

use incremental_pratt_poc::incremental::incremental_parse_with_cache_recovering;
use incremental_pratt_poc::recovery::global_optimal_repair;
use incremental_pratt_poc::{parse, Edit, RepairCosts, ReuseCache};

fn recovering(
    old: &str,
    edit: Edit,
) -> incremental_pratt_poc::incremental::IncrementalRecovery {
    let old_tree = parse(old).expect("old source parses");
    let cache = ReuseCache::build(&old_tree, old);
    incremental_parse_with_cache_recovering(&cache, old, &edit)
}

/// For a breaking edit: recovery produces repairs, applies to `new_src`, and
/// is globally cost-optimal (same cost the from-scratch oracle finds).
fn assert_broken_recovery_optimal(old: &str, edit: Edit, expected_new: &str) {
    let r = recovering(old, edit);
    assert_eq!(r.new_src, expected_new, "new_src mismatch");
    assert!(!r.repairs.is_empty(), "expected repairs for broken edit on {old:?}");
    let global = global_optimal_repair(&r.new_src);
    assert_eq!(
        RepairCosts::default().total(&r.repairs),
        global.cost,
        "incremental recovery not cost-optimal on {:?}: repairs {:?}",
        r.new_src,
        r.repairs
    );
}

#[test]
fn clean_edit_uses_reuse_no_repairs() {
    // `a + b * c` -> `a + bb * c`: still valid, so the reuse path is taken.
    let r = recovering("a + b * c", Edit { start: 4, end: 5, replacement: "bb".to_string() });
    assert!(r.repairs.is_empty(), "clean edit should need no repairs");
    assert!(r.region.is_none(), "clean edit should not enter a recovery region");
    assert_eq!(r.new_src, "a + bb * c");
    assert_eq!(
        r.tree.unparse(),
        parse(&r.new_src).unwrap().unparse(),
        "reuse path must match a fresh parse"
    );
}

#[test]
fn breaking_edit_missing_closer() {
    // Delete the `)` from `(a + b)` -> `(a + b`.
    assert_broken_recovery_optimal(
        "(a + b)",
        Edit { start: 6, end: 7, replacement: String::new() },
        "(a + b",
    );
}

#[test]
fn breaking_edit_missing_operand() {
    // Delete `b` from `a + b` -> `a + `.
    assert_broken_recovery_optimal(
        "a + b",
        Edit { start: 4, end: 5, replacement: String::new() },
        "a + ",
    );
}

#[test]
fn breaking_edit_missing_operator() {
    // Delete `+ ` from `a + b` -> `a b` (two adjacent atoms).
    assert_broken_recovery_optimal(
        "a + b",
        Edit { start: 2, end: 4, replacement: String::new() },
        "a b",
    );
}

#[test]
fn breaking_edit_stray_closer() {
    // Append a stray `)` to `a + b` -> `a + b)`.
    assert_broken_recovery_optimal(
        "a + b",
        Edit { start: 5, end: 5, replacement: ")".to_string() },
        "a + b)",
    );
}

#[test]
fn breaking_edit_inside_parens_stays_local() {
    // Error localized inside an intact paren, with a long tail outside that
    // the recovery region should exclude: `(a + ) + b + c + d`.
    let r = recovering(
        "(a + b) + c + d",
        // delete `b` inside the parens -> `(a + ) + c + d`
        Edit { start: 5, end: 6, replacement: String::new() },
    );
    assert!(!r.repairs.is_empty());
    let global = global_optimal_repair(&r.new_src);
    assert_eq!(RepairCosts::default().total(&r.repairs), global.cost);
    // The repair was localized to a region, not escalated to the whole input.
    assert!(r.region.is_some(), "expected a local recovery region");
}
