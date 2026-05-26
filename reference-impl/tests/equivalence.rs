//! Property-based equivalence: incremental_parse(old_tree, old_src, edit)
//! must agree with parse(edit.apply(old_src)) on the resulting AST
//! (compared by `unparse` — which captures structure but not metadata).

use incremental_pratt_poc::{incremental_parse, parse, Edit};

fn check_equiv(old_src: &str, edit: Edit) {
    let new_src = edit.apply(old_src);
    let fresh = match parse(&new_src) {
        Ok(n) => n,
        Err(_) => return, // skip if the new source doesn't parse
    };
    let old_tree = match parse(old_src) {
        Ok(n) => n,
        Err(_) => return,
    };
    let (incr, returned_new_src, _stats) =
        incremental_parse(&old_tree, old_src, &edit).expect("incremental should parse");
    assert_eq!(returned_new_src, new_src);
    // By default the incremental tree is structurally identical to fresh, so we
    // compare strict `unparse()`. With the `chain_splice` feature the general
    // splice may regroup an associativity-conflict chain (operand
    // insert/delete), which is sound only up to ≈ (reassociation) — Paper 2 §5
    // — so we compare `unparse_normalized()`, which flattens those chains.
    // (Strict equality implies normalized equality, so this is the right
    // per-config check.)
    #[cfg(not(feature = "chain_splice"))]
    let (got, want) = (incr.unparse(), fresh.unparse());
    #[cfg(feature = "chain_splice")]
    let (got, want) = (incr.unparse_normalized(), fresh.unparse_normalized());
    assert_eq!(
        got, want,
        "mismatch on old=`{}` edit `{}..{}` -> `{}` => new=`{}`",
        old_src, edit.start, edit.end, edit.replacement, new_src
    );
}

#[test]
fn touch_a_leaf() {
    // Old: `a + b * c`. Edit: replace `b` with `bb`.
    check_equiv(
        "a + b * c",
        Edit {
            start: 4,
            end: 5,
            replacement: "bb".to_string(),
        },
    );
}

#[test]
fn touch_an_operator_same_precedence() {
    // Old: `a + b + c`. Edit: replace `+` (first one) with `-`.
    check_equiv(
        "a + b + c",
        Edit {
            start: 2,
            end: 3,
            replacement: "-".to_string(),
        },
    );
}

#[test]
fn touch_an_operator_higher_precedence() {
    // Old: `a + b * c`. Edit: replace `+` with `*` — restructure required.
    check_equiv(
        "a + b * c",
        Edit {
            start: 2,
            end: 3,
            replacement: "*".to_string(),
        },
    );
}

#[test]
fn touch_an_operator_lower_precedence() {
    // Old: `a * b + c`. Edit: replace `*` with `+` — restructure required.
    check_equiv(
        "a * b + c",
        Edit {
            start: 2,
            end: 3,
            replacement: "+".to_string(),
        },
    );
}

#[test]
fn insert_subexpression() {
    // Old: `a + c`. Edit: insert `b *` between `+` and `c`.
    check_equiv(
        "a + c",
        Edit {
            start: 4,
            end: 4,
            replacement: "b * ".to_string(),
        },
    );
}

#[test]
fn delete_subexpression() {
    check_equiv(
        "a + b * c",
        Edit {
            start: 3,
            end: 8,
            replacement: "".to_string(),
        },
    );
}

#[test]
fn edit_inside_parens() {
    check_equiv(
        "(a + b) * c",
        Edit {
            start: 3,
            end: 4,
            replacement: "-".to_string(),
        },
    );
}

#[test]
fn edit_changing_paren_to_atom() {
    check_equiv(
        "(a + b) * c",
        Edit {
            start: 0,
            end: 7,
            replacement: "x".to_string(),
        },
    );
}

#[test]
fn right_assoc_edit() {
    check_equiv(
        "a ^ b ^ c",
        Edit {
            start: 4,
            end: 5,
            replacement: "bb".to_string(),
        },
    );
}

#[test]
fn ternary_edit_in_then() {
    check_equiv(
        "x ? a + b : c",
        Edit {
            start: 6,
            end: 7,
            replacement: "*".to_string(),
        },
    );
}

#[test]
fn no_op_edit() {
    // Replace `b` with `b` — should obviously still work.
    check_equiv(
        "a + b * c",
        Edit {
            start: 4,
            end: 5,
            replacement: "b".to_string(),
        },
    );
}

#[test]
fn ident_extension_at_end() {
    // Old: `a`. Insert `x` at end. New: `ax` — one ident.
    // (The bug the byte-boundary check catches.)
    check_equiv(
        "a",
        Edit {
            start: 1,
            end: 1,
            replacement: "x".to_string(),
        },
    );
}

#[test]
fn ident_extension_at_start() {
    // Old: `b`. Insert `a` at start. New: `ab` — one ident.
    check_equiv(
        "b",
        Edit {
            start: 0,
            end: 0,
            replacement: "a".to_string(),
        },
    );
}

#[test]
fn operator_merge_lt_eq() {
    // Old: `a < b`. Replace ` ` after `<` with `=`. New: `a <= b`.
    check_equiv(
        "a < b",
        Edit {
            start: 3,
            end: 4,
            replacement: "=".to_string(),
        },
    );
}

// ---- Context-dependent `(` role-flip (nud grouping vs led call) ----
// Targets the corner M4 surfaced for recovery: an edit that flips a token's
// nud/led role next to a cached `(` subtree. Reuse (Theorem 3.6) must still
// equal a fresh parse. `check_equiv` asserts incremental == fresh, so a
// wrongly-reused cached Paren node would make these fail.

#[test]
fn reuse_paren_to_call_adjacent() {
    // `a + (x)`: `(x)` is a grouping paren (cached). Delete `+ ` so `a` and
    // `(x)` become adjacent -> `a (x)`, which parses as the call `a(x)`.
    check_equiv("a + (x)", Edit { start: 2, end: 4, replacement: String::new() });
}

#[test]
fn reuse_paren_to_call_whitespace_separated() {
    // The dangerous corner: delete only `+`, leaving the space.
    // `a + (x)` -> `a  (x)` -> call `a(x)`. The byte immediately before the
    // `(x)` span is a space in BOTH versions, so the tokenization-boundary
    // condition does NOT fire — reuse soundness here is structural, not
    // boundary-guarded.
    check_equiv("a + (x)", Edit { start: 2, end: 3, replacement: String::new() });
}

#[test]
fn reuse_call_to_paren() {
    // `a(x)` (call args) -> `a*(x)` -> `a * (x)`, where `(x)` is now grouping.
    check_equiv("a(x)", Edit { start: 1, end: 1, replacement: "*".to_string() });
}

#[test]
fn reuse_nested_paren_role_flip() {
    // Cached `(a + b)` grouping paren becomes call args when the `+` before
    // it is deleted: `g + (a + b) * c` -> `g (a + b) * c` -> `g(a + b) * c`.
    check_equiv("g + (a + b) * c", Edit { start: 2, end: 4, replacement: String::new() });
}

#[test]
fn reuse_call_to_paren_keep_inner() {
    // `f(a + b)` call -> `f + (a + b)`: `(a + b)` flips to a grouping paren,
    // and the inner `a + b` subtree is a reuse candidate across the flip.
    check_equiv("f(a + b)", Edit { start: 1, end: 1, replacement: " + ".to_string() });
}

#[test]
fn ident_splits_into_two() {
    // Old: `ab + c`. Replace `b` with ` b` — `a b + c`. Now `a` and `b`
    // are separate idents, which should be a parse error.
    let src = "ab + c";
    let edit = Edit {
        start: 1,
        end: 2,
        replacement: " b".to_string(),
    };
    // Both fresh and incremental should error (TrailingTokens on `b`).
    let new_src = edit.apply(src);
    assert_eq!(new_src, "a b + c");
    let fresh = parse(&new_src);
    assert!(fresh.is_err());
    let old_tree = parse(src).unwrap();
    let incr = incremental_parse(&old_tree, src, &edit);
    assert!(incr.is_err());
}

mod proptests {
    use super::*;
    use proptest::prelude::*;

    /// Generate a small valid expression source string.
    fn arb_expr() -> impl Strategy<Value = String> {
        // Atoms: single-letter idents or 1-2 digit ints.
        let atom = prop_oneof![
            "[a-d]",
            "[0-9]",
            "[0-9][0-9]",
        ];

        // Recursive: depth-bounded random nesting.
        atom.prop_recursive(
            4,   // max depth
            48,  // max nodes
            6,   // items per collection (unused here)
            |inner| {
                prop_oneof![
                    (inner.clone(), prop_oneof![
                        Just("+"), Just("-"), Just("*"), Just("/"),
                        Just("&&"), Just("||"), Just("==")
                    ], inner.clone())
                        .prop_map(|(l, op, r)| format!("{} {} {}", l, op, r)),
                    inner.clone().prop_map(|c| format!("- {}", c)),
                    inner.clone().prop_map(|c| format!("! {}", c)),
                    inner.clone().prop_map(|c| format!("({})", c)),
                    // Postfix forms: function call, member access, indexing.
                    // Member name and call args use single-letter idents so
                    // the generator doesn't recurse unboundedly via field
                    // access. Indexing recurses into a nested expression.
                    inner.clone().prop_map(|c| format!("{}()", c)),
                    inner.clone().prop_map(|c| format!("{}(x)", c)),
                    (inner.clone(), inner.clone())
                        .prop_map(|(c, a)| format!("{}({})", c, a)),
                    inner.clone().prop_map(|c| format!("{}.f", c)),
                    inner.clone().prop_map(|c| format!("{}.b", c)),
                    (inner.clone(), inner.clone())
                        .prop_map(|(c, i)| format!("{}[{}]", c, i)),
                ]
            },
        )
    }

    /// Given a source string, generate an arbitrary edit on it.
    fn arb_edit(src: &str) -> impl Strategy<Value = Edit> {
        let len = src.len() as u32;
        // Generate (start, end) range within source, and a replacement.
        let starts = 0u32..=len;
        let lens = 0u32..=len.min(8); // small edits
        let replacements = prop_oneof![
            Just("".to_string()),
            Just("x".to_string()),
            Just("42".to_string()),
            Just("+".to_string()),
            Just("*".to_string()),
            Just("(y)".to_string()),
            Just(" + z ".to_string()),
            Just(".f".to_string()),
            Just("(a)".to_string()),
            Just("[i]".to_string()),
        ];
        (starts, lens, replacements).prop_map(move |(s, l, r)| {
            let end = (s + l).min(len);
            Edit {
                start: s,
                end,
                replacement: r,
            }
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 5000, .. ProptestConfig::default() })]

        #[test]
        fn random_edits_preserve_parse(src in arb_expr(), seed in any::<[u8; 32]>()) {
            // Use seed to drive edit selection deterministically via a
            // nested runner. (proptest's flat-map composition with a
            // dependent strategy is awkward; this is the simple route.)
            use proptest::strategy::ValueTree;
            use proptest::test_runner::{Config, TestRunner};
            let mut runner = TestRunner::new_with_rng(Config::default(),
                proptest::test_runner::TestRng::from_seed(
                    proptest::test_runner::RngAlgorithm::ChaCha,
                    &seed,
                ));
            let edit = arb_edit(&src).new_tree(&mut runner).unwrap().current();
            check_equiv(&src, edit);
        }
    }
}
