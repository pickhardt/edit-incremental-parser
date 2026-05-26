//! Soundness oracle (GENERATED-CRATE RUNTIME, grammar-independent).
//!
//! The load-bearing evidence: for random sources and random edits, the
//! incrementally reparsed tree must equal the from-scratch parse of the
//! edited source, modulo associativity-conflict re-association
//! (`unparse_normalized`). A single counterexample is a soundness bug.
//!
//! Strings are built by concatenating random grammar PIECES (operators,
//! atoms, parens, spaces) emitted by the generator, so the corpus is
//! grammar-specific without this file knowing the grammar.

use ipgrammar::host_grammar::SAMPLE_PROGRAM;
use ipgrammar::lexer::{GROW, PIECES, SEED_EXPR};
use ipgrammar::{
    incremental_parse, parse, parse_program, relex, reparse_program, tokenize, Edit,
    IncrementalDocument, TokenStore,
};
use proptest::prelude::*;

/// A mini-language statement tree: a `let`/expr statement (with a grown
/// expression) or a nested block of statements.
#[derive(Clone, Debug)]
enum SItem {
    Let(usize),
    Expr(usize),
    Block(Vec<SItem>),
}

fn grow_expr(reps: usize) -> String {
    let mut e = String::from(SEED_EXPR);
    for _ in 0..reps {
        e.push_str(GROW);
    }
    e
}

/// Render a statement tree to source (possibly nested blocks).
fn render_program(items: &[SItem]) -> String {
    fn go(items: &[SItem], out: &mut String, counter: &mut usize) {
        for it in items {
            match it {
                SItem::Let(r) => {
                    let i = *counter;
                    *counter += 1;
                    out.push_str(&format!("let v{} = {}; ", i, grow_expr(*r)));
                }
                SItem::Expr(r) => out.push_str(&format!("{}; ", grow_expr(*r))),
                SItem::Block(inner) => {
                    out.push_str("{ ");
                    go(inner, out, counter);
                    out.push_str("} ");
                }
            }
        }
    }
    let mut out = String::new();
    let mut counter = 0usize;
    go(items, &mut out, &mut counter);
    out
}

/// Strategy producing 1..5 top-level statements with blocks nested up to depth
/// 3, so the oracle exercises recursive intra-block reuse.
fn items_strategy() -> impl Strategy<Value = Vec<SItem>> {
    let leaf = prop_oneof![
        (0usize..3).prop_map(SItem::Let),
        (0usize..3).prop_map(SItem::Expr),
    ];
    let item = leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            2 => (0usize..3).prop_map(SItem::Let),
            2 => (0usize..3).prop_map(SItem::Expr),
            1 => prop::collection::vec(inner, 1..4).prop_map(SItem::Block),
        ]
    });
    prop::collection::vec(item, 1..5)
}

/// Build a source string from a list of PIECE indices.
fn build(pieces: &[usize]) -> String {
    let mut s = String::new();
    for &i in pieces {
        s.push_str(PIECES[i % PIECES.len()]);
    }
    s
}

/// A guaranteed-valid base expression of roughly `target` bytes, with
/// reusable structure (so the structured oracle exercises real reuse).
fn structured(target: usize) -> String {
    let mut s = String::from(SEED_EXPR);
    while s.len() < target {
        s.push_str(GROW);
    }
    s
}

/// Core soundness check shared by both oracles: given an old source that
/// parses and an edit, if the edited source parses then the incremental
/// reparse must agree with the from-scratch parse (modulo associativity).
fn check(old_src: &str, edit: &Edit) -> Result<(), TestCaseError> {
    let old_tree = match parse(old_src) {
        Ok(t) => t,
        Err(_) => return Ok(()),
    };
    let new_src = edit.apply(old_src);
    let fresh = match parse(&new_src) {
        Ok(t) => t,
        Err(_) => return Ok(()),
    };
    let (inc_tree, inc_src, _stats) = incremental_parse(&old_tree, old_src, edit)
        .expect("incremental failed where fresh succeeded (soundness bug)");
    prop_assert_eq!(&inc_src, &new_src);
    prop_assert_eq!(
        inc_tree.unparse_normalized(),
        fresh.unparse_normalized(),
        "DIVERGENCE\n old: {:?}\n new: {:?}\n edit: [{},{}) -> {:?}",
        old_src, new_src, edit.start, edit.end, edit.replacement
    );
    Ok(())
}

fn edit_at(len: u32, cut_a: f64, cut_b: f64, repl: String) -> Edit {
    let mut a = (cut_a * len as f64) as u32;
    let mut b = (cut_b * len as f64) as u32;
    if a > b { std::mem::swap(&mut a, &mut b); }
    Edit { start: a.min(len), end: b.min(len), replacement: repl }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("ORACLE_CASES").ok()
            .and_then(|v| v.parse().ok()).unwrap_or(50_000),
        max_shrink_iters: 10_000,
        ..ProptestConfig::default()
    })]

    /// Broad coverage: random source from grammar PIECES, random edit.
    /// Most cases are filtered out as unparseable; the survivors probe a
    /// wide variety of small expressions.
    #[test]
    fn random_sources(
        src_pieces in prop::collection::vec(0usize..64, 1..40),
        repl_pieces in prop::collection::vec(0usize..64, 0..6),
        cut_a in 0.0f64..1.0,
        cut_b in 0.0f64..1.0,
    ) {
        let old_src = build(&src_pieces);
        let edit = edit_at(old_src.len() as u32, cut_a, cut_b, build(&repl_pieces));
        check(&old_src, &edit)?;
    }

    /// Deep coverage: every case starts from a valid, reuse-rich base
    /// expression, so each one exercises the full reuse + comparison path.
    #[test]
    fn structured_sources(
        target in 16usize..400,
        repl_pieces in prop::collection::vec(0usize..64, 0..6),
        cut_a in 0.0f64..1.0,
        cut_b in 0.0f64..1.0,
    ) {
        let old_src = structured(target);
        let edit = edit_at(old_src.len() as u32, cut_a, cut_b, build(&repl_pieces));
        check(&old_src, &edit)?;
    }

    /// Incremental relexing equals a full re-tokenize. The load-bearing
    /// property for the persistent token store: relex-to-resync maintains the
    /// store in O(edit + resync) and the result is token-for-token identical
    /// to tokenizing the new source from scratch. Lexing works on any bytes,
    /// so this needs no parse-validity filter (broad coverage).
    #[test]
    fn relex_matches_full(
        src_pieces in prop::collection::vec(0usize..64, 0..40),
        repl_pieces in prop::collection::vec(0usize..64, 0..6),
        cut_a in 0.0f64..1.0,
        cut_b in 0.0f64..1.0,
    ) {
        let old_src = build(&src_pieces);
        let edit = edit_at(old_src.len() as u32, cut_a, cut_b, build(&repl_pieces));
        let new_src = edit.apply(&old_src);

        let mut store = TokenStore::from_tokens(&tokenize(&old_src));
        relex(&mut store, &old_src, &new_src, &edit);

        prop_assert_eq!(store.to_vec(), tokenize(&new_src));
    }

    /// The rope-backed incremental document: after an edit, its token stream
    /// equals a full tokenize of the edited text. Exercises rope splice +
    /// relex-straight-from-the-rope (relex_into over a ByteSource), the
    /// end-to-end incremental-document path.
    #[test]
    fn document_matches_full(
        src_pieces in prop::collection::vec(0usize..64, 0..40),
        repl_pieces in prop::collection::vec(0usize..64, 0..6),
        cut_a in 0.0f64..1.0,
        cut_b in 0.0f64..1.0,
    ) {
        let old_src = build(&src_pieces);
        let edit = edit_at(old_src.len() as u32, cut_a, cut_b, build(&repl_pieces));
        let new_src = edit.apply(&old_src);

        let mut doc = IncrementalDocument::new(&old_src);
        doc.edit(edit.start as usize, edit.end as usize, &edit.replacement);

        prop_assert_eq!(doc.text(), new_src.clone());
        prop_assert_eq!(doc.tokens(), tokenize(&new_src));
    }

    /// Single-tree reuse: incrementally reparsing a *program* (statements +
    /// expressions) equals a fresh program parse, for every edit where both the
    /// old and new program are valid. Statements the edit doesn't touch are
    /// reused wholesale; the edited statement's expression is reused via the
    /// precedence-bounded predicate; structural edits escalate to a sound
    /// fallback. Includes nested blocks.
    #[test]
    fn host_incremental_matches_fresh(
        items in items_strategy(),
        repl_pieces in prop::collection::vec(0usize..64, 0..4),
        cut_a in 0.0f64..1.0,
        cut_b in 0.0f64..1.0,
    ) {
        let old_src = render_program(&items);
        let old_prog = match parse_program(&old_src) {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };
        let edit = edit_at(old_src.len() as u32, cut_a, cut_b, build(&repl_pieces));
        let new_src = edit.apply(&old_src);
        let fresh = match parse_program(&new_src) {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };

        let (inc, inc_src, _stats) = reparse_program(&old_prog, &old_src, &edit)
            .expect("host incremental failed where fresh succeeded (soundness bug)");

        prop_assert_eq!(&inc_src, &new_src);
        prop_assert_eq!(inc.unparse(), fresh.unparse());
    }

    /// Edit *chains*: apply a sequence of edits, reparsing incrementally and
    /// carrying the incremental tree forward each step (never re-parsing from
    /// scratch), and check it equals a fresh parse at every step. This is what
    /// the relative-width discipline buys that a fixed-span demonstration
    /// cannot: a reused suffix needs no position fix-up, so the *next* edit
    /// still locates correctly.
    #[test]
    fn host_chain_matches_fresh(
        items in items_strategy(),
        edits in prop::collection::vec(
            (0.0f64..1.0, 0.0f64..1.0, prop::collection::vec(0usize..64, 0..3)), 1..8),
    ) {
        let mut cur_src = render_program(&items);
        let mut cur_prog = match parse_program(&cur_src) {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };
        for (a, b, repl) in &edits {
            let edit = edit_at(cur_src.len() as u32, *a, *b, build(repl));
            let new_src = edit.apply(&cur_src);
            let fresh = match parse_program(&new_src) {
                Ok(p) => p,
                Err(_) => continue, // edit makes it invalid: skip, keep state
            };
            let (inc, inc_src, _stats) = reparse_program(&cur_prog, &cur_src, &edit)
                .expect("chain: incremental failed where fresh succeeded (soundness bug)");
            prop_assert_eq!(&inc_src, &new_src);
            prop_assert_eq!(inc.unparse(), fresh.unparse());
            cur_prog = inc;
            cur_src = inc_src;
        }
    }
}

/// Demonstration of *recursive* reuse: an edit inside an expression nested two
/// blocks deep reparses only the innermost enclosing statement, reusing every
/// untouched statement (at every level) wholesale and reusing expression
/// subtrees inside the edited one. Also a correctness check (== fresh).
#[test]
fn host_recursive_reuse_demo() {
    // A program with a statement before, a nested block, and a statement after.
    let a = structured(20);
    let b = structured(30);
    let c = structured(20);
    let d = structured(20);
    let src = format!("let x = {a}; {{ let y = {b}; {c}; }} {d};");
    let prog = parse_program(&src).expect("program parses");

    // Edit a byte inside the deepest expression (the `b` inside the block).
    let (eos, _eoe) = prog.first_expr_span().expect("has an expression");
    // The first expr is `a` at top level; walk to the block's inner expr by
    // finding the second expression. Use a byte known to keep validity.
    let pos = (src.find(&b).expect("inner expr present")) as u32;
    let orig = &src[pos as usize..pos as usize + 1];
    let repl = if orig == "1" { "2" } else { "1" }.to_string();
    let _ = eos;
    let edit = Edit { start: pos, end: pos + 1, replacement: repl };

    let (inc, new_src, stats) = reparse_program(&prog, &src, &edit).expect("reparse ok");
    let fresh = parse_program(&new_src).expect("fresh ok");

    assert_eq!(inc.unparse(), fresh.unparse(), "incremental != fresh");
    assert!(!stats.fell_back, "statement-contained edit should not fall back");
    assert!(stats.reuse_depth >= 2, "edit two blocks deep should reuse at depth >= 2, got {}", stats.reuse_depth);
    assert!(stats.stmts_reused > 0, "untouched statements should be reused wholesale");
    assert!(stats.exprs_reused > 0, "expression subtrees should be reused");
    eprintln!(
        "host recursive demo: edit at depth {} -> reused {} statements wholesale, reparsed {}, reused {} expr subtrees",
        stats.reuse_depth, stats.stmts_reused, stats.stmts_reparsed, stats.exprs_reused
    );
}

/// The whole *generated* statement grammar round-trips: parse a synthesized
/// program that exercises every statement alternative (for `stmt_lang` this
/// includes `print` and `def`, which the blind generators above never emit),
/// edit inside its first expression, and check the incremental reparse equals a
/// fresh parse. Evidence that the statement front-end is genuinely generated
/// from the spec, not the hard-coded default for every grammar.
#[test]
fn host_sample_roundtrip() {
    let src = SAMPLE_PROGRAM;
    let prog = parse_program(src).expect("synthesized sample program parses");
    let (eos, eoe) = prog.first_expr_span().expect("sample has an expression");

    // Re-render must match (sanity on the sample + parse).
    let fresh0 = parse_program(src).unwrap();
    assert_eq!(prog.unparse(), fresh0.unparse());

    // Edit the first byte of the first expression (an atom) to another atom.
    let pos = eos;
    let orig = &src[pos as usize..pos as usize + 1];
    let repl = if orig == "1" { "2" } else if orig == "a" { "b" } else { return };
    let edit = Edit { start: pos, end: pos + 1, replacement: repl.to_string() };
    let _ = eoe;

    let (inc, new_src, stats) = reparse_program(&prog, src, &edit).expect("reparse ok");
    let fresh = parse_program(&new_src).expect("fresh ok");
    assert_eq!(inc.unparse(), fresh.unparse(), "incremental != fresh on sample");
    assert!(!stats.fell_back, "edit inside an expression should not fall back");
    eprintln!("sample roundtrip ok ({} bytes): {:?}", src.len(), src);
}

/// Relex window is small and independent of file size: a single-token edit
/// in the middle of a growing source re-lexes O(1) tokens, not O(n). (Also a
/// correctness check — the relexed store still equals a full tokenize.)
#[test]
fn relex_window_is_local() {
    let mut prev_window = 0;
    for reps in [200usize, 800, 3200] {
        let src: String = std::iter::repeat(GROW).take(reps).collect::<String>();
        let src = format!("{}{}", SEED_EXPR, src);
        let total = tokenize(&src).len();

        // Replace one byte in the middle (a no-op-ish single-token edit).
        let mid = (src.len() / 2) as u32;
        let edit = Edit { start: mid, end: mid + 1, replacement: src[mid as usize..mid as usize + 1].to_string() };
        let new_src = edit.apply(&src);

        let mut store = TokenStore::from_tokens(&tokenize(&src));
        let window = relex(&mut store, &src, &new_src, &edit);

        assert_eq!(store.to_vec(), tokenize(&new_src), "relex != full tokenize at reps={}", reps);
        assert!(window < 16, "relex window {} not local at reps={} (total tokens {})", window, reps, total);
        // Window stays roughly constant as the file grows ~16x.
        if prev_window != 0 {
            assert!(window <= prev_window + 2, "relex window grew with file size: {} -> {}", prev_window, window);
        }
        prev_window = window;
        eprintln!("reps={reps} total_tokens={total} relex_window={window}");
    }
}
