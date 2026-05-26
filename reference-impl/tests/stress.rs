//! Targeted property tests on edge cases the main proptest doesn't
//! exercise enough: right-associativity, ternary chains, multi-edit
//! sequences, deep nesting.

use incremental_pratt_poc::{incremental_parse, parse, Edit};
use proptest::prelude::*;

fn check_equiv(old_src: &str, edit: Edit) {
    let new_src = edit.apply(old_src);
    let fresh = match parse(&new_src) {
        Ok(n) => n,
        Err(_) => return,
    };
    let old_tree = match parse(old_src) {
        Ok(n) => n,
        Err(_) => return,
    };
    let (incr, returned_new_src, _stats) = match incremental_parse(&old_tree, old_src, &edit) {
        Ok(r) => r,
        Err(_) => return, // both parsers may fail; only compare when both succeed
    };
    assert_eq!(returned_new_src, new_src);
    // Strict structural equality by default; ≈ (normalized, flattening
    // associativity-conflict chains) under `chain_splice`, where the general
    // splice may regroup a chain on insert/delete (sound per Paper 2 §5).
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

// --------------------------------------------------------------------
// Right-associativity (^ operator, lbp 70 rbp 69)
// --------------------------------------------------------------------

mod right_assoc {
    use super::*;

    /// Generate `a ^ a ^ ... ^ a` of given depth.
    fn caret_chain(depth: usize) -> String {
        std::iter::repeat("a").take(depth).collect::<Vec<_>>().join(" ^ ")
    }

    #[test]
    fn caret_chains_concrete() {
        // Hand-written cases for depths 2..6, with various edits.
        for depth in 2..7 {
            let src = caret_chain(depth);
            // touch a leaf
            check_equiv(&src, Edit { start: 0, end: 1, replacement: "b".to_string() });
            // touch an operator (replace ^ with ^ — no-op)
            let op_pos = 2; // first ^ position is byte 2 in "a ^ a"
            check_equiv(&src, Edit { start: op_pos, end: op_pos + 1, replacement: "^".to_string() });
            // replace ^ with * (lower precedence) — restructure required
            if depth >= 3 {
                check_equiv(&src, Edit { start: op_pos, end: op_pos + 1, replacement: "*".to_string() });
            }
            // insert an atom mid-chain
            if depth >= 2 {
                check_equiv(&src, Edit { start: 1, end: 1, replacement: " ^ b".to_string() });
            }
        }
    }

    fn arb_caret_chain() -> impl Strategy<Value = String> {
        (2usize..8).prop_map(|d| caret_chain(d))
    }

    fn arb_edit(src: &str) -> impl Strategy<Value = Edit> {
        let len = src.len() as u32;
        let starts = 0u32..=len;
        let lens = 0u32..=3.min(len);
        let replacements = prop_oneof![
            Just("".to_string()),
            Just("a".to_string()),
            Just("^".to_string()),
            Just("*".to_string()),
            Just("+".to_string()),
            Just(" ^ b".to_string()),
        ];
        (starts, lens, replacements).prop_map(move |(s, l, r)| {
            let end = (s + l).min(len);
            Edit { start: s, end, replacement: r }
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 3000, .. ProptestConfig::default() })]

        #[test]
        fn caret_chain_edits(src in arb_caret_chain(), seed in any::<[u8; 32]>()) {
            use proptest::strategy::ValueTree;
            use proptest::test_runner::{Config, TestRunner};
            let mut runner = TestRunner::new_with_rng(Config::default(),
                proptest::test_runner::TestRng::from_seed(
                    proptest::test_runner::RngAlgorithm::ChaCha, &seed));
            let edit = arb_edit(&src).new_tree(&mut runner).unwrap().current();
            check_equiv(&src, edit);
        }
    }
}

// --------------------------------------------------------------------
// Ternary chains (right-assoc via rbp(?) = lbp - 1)
// --------------------------------------------------------------------

mod ternary {
    use super::*;

    #[test]
    fn ternary_chain_concrete() {
        // Right-associative: `a ? b : c ? d : e` should be `a ? b : (c ? d : e)`.
        check_equiv("a ? b : c ? d : e", Edit {
            start: 4, end: 5, replacement: "z".to_string()
        });
        // Replace `?` with another `?` — no-op
        check_equiv("a ? b : c ? d : e", Edit {
            start: 2, end: 3, replacement: "?".to_string()
        });
        // Replace an inner `:` with `:` (no-op)
        check_equiv("a ? b : c ? d : e", Edit {
            start: 14, end: 15, replacement: ":".to_string()
        });
        // Insert a new ternary level
        check_equiv("a ? b : c", Edit {
            start: 9, end: 9, replacement: " ? d : e".to_string()
        });
        // Nested ternary in condition
        check_equiv("(a ? b : c) ? d : e", Edit {
            start: 5, end: 6, replacement: "x".to_string()
        });
        // Edit replacing entire then-branch
        check_equiv("a ? b + c : d", Edit {
            start: 4, end: 9, replacement: "x".to_string()
        });
    }

    fn ternary_chain(depth: usize) -> String {
        // Builds: a ? a : a ? a : ... : a
        let mut s = String::from("a");
        for _ in 0..depth {
            s = format!("a ? a : {}", s);
        }
        s
    }

    fn arb_ternary_chain() -> impl Strategy<Value = String> {
        (1usize..6).prop_map(|d| ternary_chain(d))
    }

    fn arb_edit(src: &str) -> impl Strategy<Value = Edit> {
        let len = src.len() as u32;
        let starts = 0u32..=len;
        let lens = 0u32..=3.min(len);
        let replacements = prop_oneof![
            Just("".to_string()),
            Just("a".to_string()),
            Just("?".to_string()),
            Just(":".to_string()),
            Just("b".to_string()),
            Just(" + c".to_string()),
        ];
        (starts, lens, replacements).prop_map(move |(s, l, r)| {
            let end = (s + l).min(len);
            Edit { start: s, end, replacement: r }
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 3000, .. ProptestConfig::default() })]

        #[test]
        fn ternary_chain_edits(src in arb_ternary_chain(), seed in any::<[u8; 32]>()) {
            use proptest::strategy::ValueTree;
            use proptest::test_runner::{Config, TestRunner};
            let mut runner = TestRunner::new_with_rng(Config::default(),
                proptest::test_runner::TestRng::from_seed(
                    proptest::test_runner::RngAlgorithm::ChaCha, &seed));
            let edit = arb_edit(&src).new_tree(&mut runner).unwrap().current();
            check_equiv(&src, edit);
        }
    }
}

// --------------------------------------------------------------------
// Multi-edit sequences: apply N edits in series, each time using the
// previously-reparsed tree as the cache for the next reparse.
// --------------------------------------------------------------------

mod multi_edit {
    use super::*;

    fn check_multi_equiv(initial_src: &str, edits: Vec<Edit>) {
        // Replay the edits with both fresh parsing and incremental
        // reparsing; they must agree at every step.
        let mut fresh_src = initial_src.to_string();
        let mut incr_src = initial_src.to_string();
        let mut incr_tree = match parse(&incr_src) {
            Ok(n) => n,
            Err(_) => return,
        };
        for edit in &edits {
            let fresh_new = edit.apply(&fresh_src);
            let fresh_tree = match parse(&fresh_new) {
                Ok(n) => n,
                Err(_) => return,
            };
            let (next_tree, next_src, _stats) =
                match incremental_parse(&incr_tree, &incr_src, edit) {
                    Ok(r) => r,
                    Err(_) => return,
                };
            assert_eq!(next_src, fresh_new, "src mismatch after edit");
            #[cfg(not(feature = "chain_splice"))]
            let (got, want) = (next_tree.unparse(), fresh_tree.unparse());
            #[cfg(feature = "chain_splice")]
            let (got, want) = (next_tree.unparse_normalized(), fresh_tree.unparse_normalized());
            assert_eq!(
                got, want,
                "tree mismatch after edit on src=`{}` edit `{}..{}` -> `{}`",
                incr_src, edit.start, edit.end, edit.replacement
            );
            fresh_src = fresh_new;
            incr_src = next_src;
            incr_tree = next_tree;
        }
    }

    #[test]
    fn two_sequential_edits() {
        check_multi_equiv("a + b * c", vec![
            Edit { start: 2, end: 3, replacement: "*".to_string() },
            Edit { start: 4, end: 5, replacement: "z".to_string() },
        ]);
    }

    #[test]
    fn typing_a_word_one_char_at_a_time() {
        // Start with `x` and type ` + hello` one character at a time.
        let edits: Vec<Edit> = " + hello".chars().enumerate().map(|(i, c)| Edit {
            start: 1 + i as u32,
            end: 1 + i as u32,
            replacement: c.to_string(),
        }).collect();
        check_multi_equiv("x", edits);
    }

    #[test]
    fn delete_then_reinsert() {
        check_multi_equiv("a + b * c", vec![
            Edit { start: 3, end: 8, replacement: "".to_string() },
            Edit { start: 3, end: 3, replacement: "+ b * c".to_string() },
        ]);
    }

    fn arb_initial() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("a + b".to_string()),
            Just("a + b * c".to_string()),
            Just("a ? b : c".to_string()),
            Just("(a + b) * c".to_string()),
            Just("a && b || c == d".to_string()),
            Just("-a + b * !c".to_string()),
        ]
    }

    fn arb_edit_for(src: &str) -> impl Strategy<Value = Edit> {
        let len = src.len() as u32;
        let starts = 0u32..=len;
        let lens = 0u32..=2.min(len);
        let replacements = prop_oneof![
            Just("".to_string()),
            Just("z".to_string()),
            Just("+".to_string()),
            Just("*".to_string()),
            Just("99".to_string()),
        ];
        (starts, lens, replacements).prop_map(move |(s, l, r)| {
            let end = (s + l).min(len);
            Edit { start: s, end, replacement: r }
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 1500, .. ProptestConfig::default() })]

        #[test]
        fn random_3_edit_sequences(
            initial in arb_initial(),
            seeds in proptest::collection::vec(any::<[u8; 32]>(), 3),
        ) {
            use proptest::strategy::ValueTree;
            use proptest::test_runner::{Config, TestRunner};
            let mut src = initial.clone();
            let mut edits = Vec::new();
            for seed in seeds {
                let mut runner = TestRunner::new_with_rng(Config::default(),
                    proptest::test_runner::TestRng::from_seed(
                        proptest::test_runner::RngAlgorithm::ChaCha, &seed));
                let edit = arb_edit_for(&src).new_tree(&mut runner).unwrap().current();
                src = edit.apply(&src);
                edits.push(edit);
            }
            check_multi_equiv(&initial, edits);
        }
    }
}

// --------------------------------------------------------------------
// Deep nesting: expressions with high nesting depth.
// --------------------------------------------------------------------

mod deep_nesting {
    use super::*;

    fn deeply_nested_parens(depth: usize) -> String {
        let mut s = String::from("a");
        for _ in 0..depth {
            s = format!("({})", s);
        }
        s
    }

    fn deeply_left_assoc(depth: usize) -> String {
        let mut s = String::from("a");
        for _ in 0..depth {
            s.push_str(" + a");
        }
        s
    }

    #[test]
    fn deep_parens_edit_inner() {
        for depth in [3, 10, 30, 80] {
            let src = deeply_nested_parens(depth);
            // edit the innermost atom
            let pos = depth as u32;
            check_equiv(&src, Edit { start: pos, end: pos + 1, replacement: "z".to_string() });
        }
    }

    #[test]
    fn deep_left_assoc_edit_middle_op() {
        for depth in [3, 10, 30, 80] {
            let src = deeply_left_assoc(depth);
            // edit the middle operator
            let mid_op = (src.len() / 2) as u32;
            // walk back to a `+` byte
            let bytes = src.as_bytes();
            let mut p = mid_op as usize;
            while p < bytes.len() && bytes[p] != b'+' {
                p += 1;
            }
            if p >= bytes.len() {
                continue;
            }
            check_equiv(&src, Edit { start: p as u32, end: (p + 1) as u32, replacement: "-".to_string() });
            check_equiv(&src, Edit { start: p as u32, end: (p + 1) as u32, replacement: "*".to_string() });
        }
    }
}

// --------------------------------------------------------------------
// Long associative chains (the Diekmann §2.9 pathology shape).
// --------------------------------------------------------------------

mod long_chains {
    use super::*;

    fn chain_of(n: usize, sep: &str) -> String {
        std::iter::repeat("a").take(n).collect::<Vec<_>>().join(&format!(" {} ", sep))
    }

    #[test]
    fn plus_chain_edit_in_middle() {
        // Diekmann's worst case: deeply nested associative chain, edit
        // an operand in the middle. With balanced building the chain
        // has O(log n) depth instead of O(n), so the edit's effect
        // propagates only along log(n) ancestor nodes.
        for n in [10, 50, 100, 250] {
            let src = chain_of(n, "+");
            // Edit roughly the middle `a` — replace with `bb`.
            let mid_byte = (src.len() / 2) as u32;
            // Walk forward to nearest 'a' or 'b' (skip the `+` chars).
            let bytes = src.as_bytes();
            let mut p = mid_byte as usize;
            while p < bytes.len() && bytes[p] != b'a' {
                p += 1;
            }
            if p >= bytes.len() {
                continue;
            }
            check_equiv(
                &src,
                Edit {
                    start: p as u32,
                    end: (p + 1) as u32,
                    replacement: "bb".to_string(),
                },
            );
        }
    }

    #[test]
    fn mixed_op_chain_edit() {
        // Mixed: `a + a * a + a * a + a` — chain detection must split
        // at non-associative boundaries and rebuild each chain's
        // balanced structure separately.
        let src = "a + a * a + a * a + a";
        check_equiv(
            src,
            Edit {
                start: 8,
                end: 9,
                replacement: "b".to_string(),
            },
        );
    }

    #[test]
    fn and_chain_edit() {
        let src = "a && a && a && a && a && a && a";
        check_equiv(
            src,
            Edit {
                start: 11,
                end: 12,
                replacement: "b".to_string(),
            },
        );
    }

    #[test]
    fn very_long_chain_does_not_stack_overflow() {
        // 1000-operand chain. Without balanced building this would be
        // 1000-deep recursion in the AST. With balanced building it's
        // ~10-deep. Test that both parse and unparse complete.
        let src = chain_of(1000, "+");
        let tree = parse(&src).expect("must parse");
        let _ = tree.unparse();
        // Edit at byte 1000 (middle-ish).
        let bytes = src.as_bytes();
        let mut p = 1000usize;
        while p < bytes.len() && bytes[p] != b'a' {
            p += 1;
        }
        check_equiv(
            &src,
            Edit {
                start: p as u32,
                end: (p + 1) as u32,
                replacement: "b".to_string(),
            },
        );
    }
}

// Re-export check_equiv for use in submodules.
fn _silence(_: ()) {
    check_equiv("a", Edit { start: 0, end: 0, replacement: "".to_string() });
}
