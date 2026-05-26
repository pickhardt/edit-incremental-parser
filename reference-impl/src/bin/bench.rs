//! Empirical comparison of reuse predicates.
//!
//! Generates random expressions of varying sizes, applies a small edit,
//! and times each reparse strategy. All incremental strategies use the
//! same Arc-shared AST so the comparison isolates predicate choice from
//! data-structure choice.
//!
//! Strategies measured:
//!   - fresh         — from-scratch parse (baseline)
//!   - prec_bounded  — our precedence-band predicate (sound)
//!   - span_la0/1/2  — naive span + N-token-lookahead (may be unsound)
//!   - roslyn_style  — sound: M_floor + 1-token lookahead
//!
//! Cache build cost is amortized across edits in a real LSP, so the
//! per-edit reparse timer excludes cache construction.

use std::time::{Duration, Instant};

use incremental_pratt_poc::{
    bench_support::{gen_atom, gen_binop, gen_field, Lcg},
    incremental_parse_with_cache, parse, roslyn_style_with_cache, span_lookahead_with_cache,
    Edit, Node, ReuseCache,
};

fn gen_expr(target_bytes: usize, mut rng: Lcg) -> String {
    fn gen(depth: u32, budget: &mut usize, rng: &mut Lcg) -> String {
        if *budget < 4 || depth > 12 { return gen_atom(rng); }
        let choice = rng.range(14);
        *budget = budget.saturating_sub(3);
        match choice {
            0..=4 => format!("{} {} {}", gen(depth+1, budget, rng), gen_binop(rng), gen(depth+1, budget, rng)),
            5 => format!("({})", gen(depth+1, budget, rng)),
            6 => format!("-{}", gen(depth+1, budget, rng)),
            7 => format!("!{}", gen(depth+1, budget, rng)),
            8 => format!("({} ? {} : {})", gen(depth+1, budget, rng), gen(depth+1, budget, rng), gen(depth+1, budget, rng)),
            // Postfix forms (~30% combined): function call, member, index.
            9  => format!("{}({})", gen(depth+1, budget, rng), gen(depth+1, budget, rng)),
            10 => format!("{}.{}", gen(depth+1, budget, rng), gen_field(rng)),
            11 => format!("{}[{}]", gen(depth+1, budget, rng), gen(depth+1, budget, rng)),
            12 => format!("{}()", gen(depth+1, budget, rng)),
            _  => format!("{}.{}", gen(depth+1, budget, rng), gen_field(rng)),
        }
    }
    let mut budget = target_bytes;
    gen(0, &mut budget, &mut rng)
}

fn gen_edit(src: &str, mut rng: Lcg) -> Edit {
    let len = src.len() as u32;
    if len == 0 { return Edit { start: 0, end: 0, replacement: "a".to_string() }; }
    let pos = rng.range(len as usize) as u32;
    match rng.range(5) {
        0 => Edit { start: pos, end: (pos+1).min(len), replacement: ((b'0' + rng.range(10) as u8) as char).to_string() },
        1 => Edit { start: pos, end: (pos+1).min(len), replacement: match rng.range(4) { 0=>'a', 1=>'b', 2=>'x', _=>'y' }.to_string() },
        2 => Edit { start: pos, end: pos, replacement: " ".to_string() },
        3 => Edit { start: pos, end: pos, replacement: ((b'0' + rng.range(10) as u8) as char).to_string() },
        _ => Edit { start: pos, end: (pos+1).min(len), replacement: match rng.range(7) { 0=>'+', 1=>'-', 2=>'*', 3=>'/', 4=>'<', 5=>'>', _=>'%' }.to_string() },
    }
}

#[derive(Default, Clone)]
struct StratResult {
    times: Vec<Duration>,
    reuse_rates: Vec<f64>,
    incorrect: u32,
    parse_failed: u32,
}
impl StratResult {
    fn median_us(&self) -> u128 {
        let mut v: Vec<_> = self.times.iter().map(|d| d.as_micros()).collect();
        v.sort();
        if v.is_empty() { 0 } else { v[v.len() / 2] }
    }
    fn mean_reuse(&self) -> f64 {
        if self.reuse_rates.is_empty() { 0.0 } else { self.reuse_rates.iter().sum::<f64>() / self.reuse_rates.len() as f64 }
    }
}

fn time_run<R>(f: impl FnOnce() -> R) -> (R, Duration) {
    let t = Instant::now();
    let r = f();
    (r, t.elapsed())
}

fn main() {
    let sizes = [200usize, 1_000, 5_000, 20_000, 80_000];
    let trials = 200;
    let mut seed: u64 = 0xdeadbeef;

    println!("=== Section 1: random expressions ===\n");
    println!("Per-edit reparse time (cache amortized across edits in real LSP).\n");

    for &size in &sizes {
        let mut fresh = StratResult::default();
        let mut prec = StratResult::default();
        let mut la0 = StratResult::default();
        let mut la1 = StratResult::default();
        let mut roslyn1 = StratResult::default();

        for _ in 0..trials {
            seed = seed.wrapping_add(0x9E3779B97F4A7C15);
            let src = gen_expr(size, Lcg(seed));
            let edit = gen_edit(&src, Lcg(seed.wrapping_add(1)));

            let old_tree = match parse(&src) {
                Ok(n) => n,
                Err(_) => continue,
            };
            let new_src = edit.apply(&src);
            let (fresh_result, fresh_time) = time_run(|| parse(&new_src));
            let fresh_node = match &fresh_result {
                Ok(n) => Some(n.clone()),
                Err(_) => { fresh.parse_failed += 1; continue; }
            };
            fresh.times.push(fresh_time);
            fresh.reuse_rates.push(0.0);

            let total_nodes = fresh_node.as_ref().unwrap().count() as f64;
            let cache = ReuseCache::build(&old_tree, &src);

            macro_rules! run {
                ($strat:ident, $call:expr) => {{
                    let (result, time) = time_run(|| $call);
                    match result {
                        Ok((node, _, stats)) => {
                            $strat.times.push(time);
                            $strat.reuse_rates.push(stats.nodes_reused as f64 / total_nodes.max(1.0));
                            if node.unparse_normalized() != fresh_node.as_ref().unwrap().unparse_normalized() {
                                $strat.incorrect += 1;
                            }
                        }
                        Err(_) => $strat.parse_failed += 1,
                    }
                }};
            }
            run!(prec, incremental_parse_with_cache(&cache, &src, &edit));
            run!(la0,  span_lookahead_with_cache(&cache, &src, &edit, 0));
            run!(la1,  span_lookahead_with_cache(&cache, &src, &edit, 1));
            run!(roslyn1, roslyn_style_with_cache(&cache, &src, &edit, 1));

            let _ = &fresh_node;
        }

        println!("=== size ≈ {} bytes, {} successful trials ===", size, fresh.times.len());
        for (label, s) in [
            ("fresh",         &fresh),
            ("prec_bounded",  &prec),
            ("span_la0",      &la0),
            ("span_la1",      &la1),
            ("roslyn_style",  &roslyn1),
        ] {
            println!(
                "  {:<16}  median {:>6} µs   reuse {:>6.1}%   incorrect {:>3}/{:<3}",
                label, s.median_us(), s.mean_reuse() * 100.0, s.incorrect, s.times.len()
            );
        }
        println!();
    }

    println!("\n=== Section 2: long associative chains (Diekmann §2.9 pathology shape) ===\n");
    println!("Source shape: `(operand) + (operand) + ... + (operand)` where each operand");
    println!("is `a * b + c * 7` (a non-trivial subexpression). Edit: change one byte inside");
    println!("one operand near the middle. This mirrors Diekmann's Java-statement-blocks case");
    println!("(long associative lists of structurally-meaningful elements).\n");

    // Each operand is a non-trivial subexpression (`a * b + c * 7`),
    // so reuse via Arc::clone of unchanged operands saves real work.
    // This is the shape that matches Diekmann's Java-statement-blocks
    // case where each list element is a full statement.
    for chain_len in [100usize, 500, 1000, 2500] {
        let operand = "(a * b + c * 7)";
        let src: String = std::iter::repeat(operand).take(chain_len).collect::<Vec<_>>().join(" + ");

        let mut fresh = StratResult::default();
        let mut prec = StratResult::default();

        for trial in 0..50 {
            // Pick a different middle-ish operand each trial so we don't
            // just hit the same byte over and over.
            let stride = src.len() / 50;
            let base = src.len() / 4 + trial * stride / 4;
            let bytes = src.as_bytes();
            // Find a byte inside an operand (not on a `+` between operands).
            let mut p = base.min(bytes.len().saturating_sub(2));
            while p < bytes.len() && (bytes[p] == b' ' || bytes[p] == b'+') {
                p += 1;
            }
            if p >= bytes.len() {
                continue;
            }
            // Replace one byte (some operand char) with `z` — local edit
            // inside one operand subtree.
            let edit = Edit {
                start: p as u32,
                end: (p + 1) as u32,
                replacement: "z".to_string(),
            };
            let old_tree = match parse(&src) { Ok(n) => n, Err(_) => continue };
            let new_src = edit.apply(&src);
            let (fresh_result, fresh_time) = time_run(|| parse(&new_src));
            let fresh_node = match fresh_result { Ok(n) => n, Err(_) => continue };
            fresh.times.push(fresh_time);
            let total_nodes = fresh_node.count() as f64;
            let cache = ReuseCache::build(&old_tree, &src);
            let (prec_result, prec_time) = time_run(|| incremental_parse_with_cache(&cache, &src, &edit));
            if let Ok((node, _, stats)) = prec_result {
                prec.times.push(prec_time);
                prec.reuse_rates.push(stats.nodes_reused as f64 / total_nodes.max(1.0));
                if node.unparse_normalized() != fresh_node.unparse_normalized() {
                    prec.incorrect += 1;
                }
            }
        }

        println!(
            "  chain={:>4} ops ({} bytes):  fresh {:>5} µs   prec_bounded {:>5} µs   speedup {:.1}×   reuse {:.1}%   incorrect {}/{}",
            chain_len, src.len(), fresh.median_us(), prec.median_us(),
            fresh.median_us() as f64 / prec.median_us().max(1) as f64,
            prec.mean_reuse() * 100.0,
            prec.incorrect, prec.times.len()
        );
    }

    let _ = Node::atom; // keep `Node` import warning-free
}
