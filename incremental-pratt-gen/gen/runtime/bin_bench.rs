//! Benchmark (GENERATED-CRATE RUNTIME, grammar-independent).
//!
//! Grows a guaranteed-valid source from the generator-emitted SEED_EXPR /
//! GROW constants, then for each size measures median fresh-parse latency
//! vs. median incremental-reparse latency (cache prebuilt, off the hot
//! path, as a production LSP would maintain it) and the subtree reuse rate.
//!
//! Output is CSV on stdout: grammar,bytes,fresh_us,reparse_us,speedup,reuse_pct

use std::time::Instant;

use ipgrammar::lexer::{ATOM, INFIX, PCLOSE, POPEN};
use ipgrammar::{incremental_reparse, parse, Edit, ReuseCache};

/// A balanced, nested expression of depth `d` (size ~2^d atoms). An edit
/// in one leaf invalidates only the O(d) paren ancestors on its spine;
/// the entire sibling subtree is reused as a single Arc::clone. This is
/// the locality the precedence-bounded predicate is designed to exploit —
/// the realistic case, as opposed to a flat operator chain.
fn nested(d: u32) -> String {
    if d == 0 {
        ATOM.to_string()
    } else {
        format!("{POPEN}{}{INFIX}{}{PCLOSE}", nested(d - 1), nested(d - 1))
    }
}

/// Byte offsets of every atom leaf, for clustered edits.
fn leaf_positions(src: &str) -> Vec<u32> {
    let a0 = ATOM.as_bytes()[0];
    src.bytes().enumerate().filter(|&(_, b)| b == a0).map(|(i, _)| i as u32).collect()
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if v.is_empty() { 0.0 } else { v[v.len() / 2] }
}

// Tiny deterministic PRNG (xorshift) so benches reproduce bit-for-bit.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn main() {
    let grammar = std::env::args().nth(1).unwrap_or_else(|| "grammar".into());
    let depths = [8u32, 11, 13]; // ~256, ~2k, ~8k atoms
    let samples = 400;

    // The buffer `apply` (string copy) is editor infrastructure, done
    // outside the timed reparse: we measure the parser+lexer hot path,
    // which is what demand-driven lexing accelerates. `lexed_pct` is the
    // fraction of the file's tokens the incremental reparse actually lexed.
    println!("grammar,bytes,fresh_us,reparse_us,speedup,reuse_pct,lexed_pct");
    for &depth in &depths {
        let src = nested(depth);
        let tree = match parse(&src) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("nested source for {} did not parse: {:?}", grammar, e);
                continue;
            }
        };
        let cache = ReuseCache::build(&tree, &src);
        let total_tokens = ipgrammar::tokenize(&src).len() as f64;
        let leaves = leaf_positions(&src);
        let other_atom = if ATOM == "1" { "2" } else { "b" };

        let mut rng = Rng(0x9E3779B97F4A7C15 ^ depth as u64);
        let mut fresh_us = Vec::new();
        let mut reparse_us = Vec::new();
        let mut reuse_ratios = Vec::new();
        let mut lexed_ratios = Vec::new();
        let mut taken = 0;
        let mut attempts = 0;

        while taken < samples && attempts < samples * 50 {
            attempts += 1;
            // Edit a single leaf atom (always keeps the source valid).
            let pos = leaves[rng.below(leaves.len())];
            let edit = Edit {
                start: pos,
                end: pos + ATOM.len() as u32,
                replacement: other_atom.to_string(),
            };
            // Editor maintains the buffer: apply outside the timed reparse.
            let new_src = edit.apply(&src);

            let t0 = Instant::now();
            let fresh = parse(&new_src);
            let f = t0.elapsed().as_secs_f64() * 1e6;
            if fresh.is_err() { continue; }

            let t1 = Instant::now();
            let inc = incremental_reparse(&cache, &src, &new_src, &edit);
            let r = t1.elapsed().as_secs_f64() * 1e6;
            let (inc_tree, stats) = match inc { Ok(v) => v, Err(_) => continue };

            let total = (stats.nodes_reused + stats.nodes_parsed).max(1);
            reuse_ratios.push(stats.nodes_reused as f64 / total as f64);
            lexed_ratios.push(stats.tokens_lexed as f64 / total_tokens);
            fresh_us.push(f);
            reparse_us.push(r);
            let _ = inc_tree;
            taken += 1;
        }

        let fm = median(fresh_us);
        let rm = median(reparse_us);
        let speedup = if rm > 0.0 { fm / rm } else { 0.0 };
        let reuse_pct = 100.0 * median(reuse_ratios);
        let lexed_pct = 100.0 * median(lexed_ratios);
        println!(
            "{},{},{:.1},{:.1},{:.2},{:.1},{:.1}",
            grammar, src.len(), fm, rm, speedup, reuse_pct, lexed_pct
        );
    }
}
