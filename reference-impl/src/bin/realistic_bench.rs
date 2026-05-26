//! Realistic edit-sequence benchmark.
//!
//! Per-edit reparse time on a single large source, with edits that
//! simulate developer typing patterns rather than uniform-random
//! adversarial single-byte mutations.
//!
//! Edit-pattern distribution (modeled after how real developers edit
//! source code):
//!   * 40% insert one character at cursor (typing)
//!   * 20% delete one character at cursor (backspace)
//!   * 15% rename: replace 1-3 byte ident-like range near cursor
//!   * 15% constant change: replace a digit-sequence near cursor
//!   * 5%  operator swap: replace an operator near cursor (same-class)
//!   * 5%  cursor jump (scroll / click elsewhere)
//!
//! Edits cluster around a maintained cursor position (most edits within
//! ±20 bytes). Each edit is applied sequentially and the cache is
//! rebuilt for the next iteration (representing an LSP that maintains
//! a cache alongside the tree).
//!
//! Reports per-edit timing distribution (median, p95, p99) for both
//! fresh-parse and precedence-bounded-incremental strategies.

use std::time::{Duration, Instant};

use incremental_pratt_poc::{
    bench_support::Lcg, incremental_parse_with_cache, parse, Edit, Node, ReuseCache,
};

/// Cursor-clustered edits use a Gaussian-ish offset distribution.
/// Wraps `Lcg::range` to produce a sum-of-two-uniforms offset.
fn clustered_offset(rng: &mut Lcg, sigma: usize) -> i64 {
    let a = rng.range(2 * sigma);
    let b = rng.range(2 * sigma);
    (a as i64 - sigma as i64) + (b as i64 - sigma as i64)
}

/// Generate a starting source: a non-trivial expression of approximately
/// the requested byte length, with mixed precedences and parens.
fn gen_base_source(target_bytes: usize, mut rng: Lcg) -> String {
    fn gen(depth: u32, budget: &mut usize, rng: &mut Lcg) -> String {
        if *budget < 4 || depth > 10 {
            return match rng.range(4) {
                0 => "a".to_string(),
                1 => "x".to_string(),
                2 => format!("{}", rng.range(100)),
                _ => "n".to_string(),
            };
        }
        let choice = rng.range(14);
        *budget = budget.saturating_sub(3);
        match choice {
            0..=3 => format!(
                "{} {} {}",
                gen(depth + 1, budget, rng),
                ["+", "-", "*", "/", "&&", "||", "==", "<", ">"][rng.range(9)],
                gen(depth + 1, budget, rng)
            ),
            4..=5 => format!("({})", gen(depth + 1, budget, rng)),
            6 => format!("-{}", gen(depth + 1, budget, rng)),
            7 => format!("!{}", gen(depth + 1, budget, rng)),
            8 => format!(
                "({} ? {} : {})",
                gen(depth + 1, budget, rng),
                gen(depth + 1, budget, rng),
                gen(depth + 1, budget, rng)
            ),
            // Postfix forms: function call, member access, indexing.
            9  => format!("{}({})", gen(depth + 1, budget, rng), gen(depth + 1, budget, rng)),
            10 => format!("{}.{}", gen(depth + 1, budget, rng),
                          ["f", "g", "field", "next"][rng.range(4)]),
            11 => format!("{}[{}]", gen(depth + 1, budget, rng), gen(depth + 1, budget, rng)),
            12 => format!("{}()", gen(depth + 1, budget, rng)),
            _  => format!("{}.{}", gen(depth + 1, budget, rng),
                          ["f", "g", "field", "next"][rng.range(4)]),
        }
    }
    let mut budget = target_bytes;
    gen(0, &mut budget, &mut rng)
}

#[derive(Clone, Copy, Debug)]
enum EditKind {
    InsertChar,
    DeleteChar,
    RenameIdent,
    ChangeConstant,
    SwapOp,
    CursorJump,
}

fn pick_edit_kind(rng: &mut Lcg) -> EditKind {
    match rng.range(100) {
        0..=39 => EditKind::InsertChar,
        40..=59 => EditKind::DeleteChar,
        60..=74 => EditKind::RenameIdent,
        75..=89 => EditKind::ChangeConstant,
        90..=94 => EditKind::SwapOp,
        _ => EditKind::CursorJump,
    }
}

/// Compose an Edit at (or near) `cursor`, returning the Edit and a new
/// cursor position. Returns `None` if no sensible edit could be made
/// (e.g., trying to delete at empty source).
fn make_edit(
    src: &str,
    cursor: usize,
    kind: EditKind,
    rng: &mut Lcg,
) -> Option<(Edit, usize)> {
    let len = src.len();
    if len == 0 {
        return Some((Edit { start: 0, end: 0, replacement: "a".to_string() }, 1));
    }
    let bytes = src.as_bytes();
    match kind {
        EditKind::InsertChar => {
            let pos = cursor.min(len);
            let c = match rng.range(8) {
                0 => 'a', 1 => 'b', 2 => 'x', 3 => 'y',
                4 => '1', 5 => '2', 6 => ' ', _ => '0',
            };
            Some((
                Edit { start: pos as u32, end: pos as u32, replacement: c.to_string() },
                pos + 1,
            ))
        }
        EditKind::DeleteChar => {
            let pos = cursor.min(len).max(1) - 1;
            Some((
                Edit { start: pos as u32, end: (pos + 1) as u32, replacement: String::new() },
                pos,
            ))
        }
        EditKind::RenameIdent => {
            // Find an ident byte near cursor.
            let mut p = cursor.min(len.saturating_sub(1));
            for _ in 0..16 {
                if p < len && bytes[p].is_ascii_lowercase() {
                    break;
                }
                let off = clustered_offset(rng, 8);
                p = ((p as i64 + off).max(0) as usize).min(len.saturating_sub(1));
            }
            if p >= len || !bytes[p].is_ascii_lowercase() {
                return None;
            }
            // Replace one ident char with another.
            let c = match rng.range(5) { 0 => 'a', 1 => 'b', 2 => 'c', 3 => 'x', _ => 'y' };
            Some((
                Edit { start: p as u32, end: (p + 1) as u32, replacement: c.to_string() },
                p + 1,
            ))
        }
        EditKind::ChangeConstant => {
            let mut p = cursor.min(len.saturating_sub(1));
            for _ in 0..16 {
                if p < len && bytes[p].is_ascii_digit() {
                    break;
                }
                let off = clustered_offset(rng, 8);
                p = ((p as i64 + off).max(0) as usize).min(len.saturating_sub(1));
            }
            if p >= len || !bytes[p].is_ascii_digit() {
                return None;
            }
            let c = (b'0' + rng.range(10) as u8) as char;
            Some((
                Edit { start: p as u32, end: (p + 1) as u32, replacement: c.to_string() },
                p + 1,
            ))
        }
        EditKind::SwapOp => {
            // Find an operator-class byte near cursor.
            let mut p = cursor.min(len.saturating_sub(1));
            let op_chars = b"+-*/<>%";
            for _ in 0..16 {
                if p < len && op_chars.contains(&bytes[p]) {
                    break;
                }
                let off = clustered_offset(rng, 8);
                p = ((p as i64 + off).max(0) as usize).min(len.saturating_sub(1));
            }
            if p >= len || !op_chars.contains(&bytes[p]) {
                return None;
            }
            // Swap with a different operator (random).
            let c = op_chars[rng.range(op_chars.len())] as char;
            Some((
                Edit { start: p as u32, end: (p + 1) as u32, replacement: c.to_string() },
                p + 1,
            ))
        }
        EditKind::CursorJump => {
            // No edit; just move cursor.
            let new_cursor = rng.range(len);
            Some((
                Edit { start: 0, end: 0, replacement: String::new() },
                new_cursor,
            ))
        }
    }
}

fn percentile(times: &[Duration], p: f64) -> Duration {
    if times.is_empty() {
        return Duration::ZERO;
    }
    let mut sorted: Vec<_> = times.iter().copied().collect();
    sorted.sort();
    let idx = ((sorted.len() as f64) * p).min(sorted.len() as f64 - 1.0) as usize;
    sorted[idx]
}

fn time_run<R>(f: impl FnOnce() -> R) -> (R, Duration) {
    let t = Instant::now();
    let r = f();
    (r, t.elapsed())
}

fn main() {
    for &target_size in &[1_000usize, 5_000, 20_000] {
        let mut src = gen_base_source(target_size, Lcg(0xfaceb00c + target_size as u64));
        // Discard if it doesn't parse.
        if parse(&src).is_err() {
            // Try a different seed.
            src = gen_base_source(target_size, Lcg(0xdeadbeef + target_size as u64));
            if parse(&src).is_err() {
                println!("(could not generate parseable source at size {})", target_size);
                continue;
            }
        }

        println!(
            "\n=== Realistic edit sequence on {}-byte source ({} bytes actual) ===",
            target_size,
            src.len()
        );

        let mut rng = Lcg(0xc0ffee ^ target_size as u64);
        let mut cursor = src.len() / 2;
        let mut tree = parse(&src).unwrap();
        let mut cache = ReuseCache::build(&tree, &src);

        let mut fresh_times: Vec<Duration> = Vec::new();
        let mut prec_reparse_times: Vec<Duration> = Vec::new();
        let mut cache_build_times: Vec<Duration> = Vec::new();
        let mut prec_total_times: Vec<Duration> = Vec::new();
        let mut applied = 0u32;
        let mut skipped_invalid = 0u32;

        let n_edits = 500;
        for _ in 0..n_edits {
            let kind = pick_edit_kind(&mut rng);
            let (edit, new_cursor) = match make_edit(&src, cursor, kind, &mut rng) {
                Some(p) => p,
                None => continue,
            };

            // No-op cursor moves: just update cursor and continue.
            if edit.start == edit.end && edit.replacement.is_empty() {
                cursor = new_cursor;
                continue;
            }

            let new_src = edit.apply(&src);

            // Time fresh.
            let (fresh_result, fresh_time) = time_run(|| parse(&new_src));
            let fresh_tree = match fresh_result {
                Ok(n) => n,
                Err(_) => { skipped_invalid += 1; continue; }
            };

            // Time precedence-bounded incremental with prebuilt cache.
            let (prec_result, prec_time) =
                time_run(|| incremental_parse_with_cache(&cache, &src, &edit));
            let (prec_tree, _, _) = match prec_result {
                Ok(r) => r,
                Err(_) => { skipped_invalid += 1; continue; }
            };

            // Correctness assertion.
            assert_eq!(
                prec_tree.unparse_normalized(),
                fresh_tree.unparse_normalized(),
                "incremental diverged from fresh on edit {:?} at cursor {}",
                edit, cursor
            );

            // Cache rebuild for next iteration — measured separately
            // so we can report both "reparse latency" (what the user
            // perceives before the editor updates) and "total per-edit
            // CPU cost" (reparse + cache maintenance, what the LSP
            // process spends in aggregate).
            src = new_src;
            tree = fresh_tree;
            let (new_cache, cache_build_time) = time_run(|| ReuseCache::build(&tree, &src));
            cache = new_cache;
            cursor = new_cursor.min(src.len());

            fresh_times.push(fresh_time);
            prec_reparse_times.push(prec_time);
            cache_build_times.push(cache_build_time);
            prec_total_times.push(prec_time + cache_build_time);
            applied += 1;
        }

        let fresh_med = percentile(&fresh_times, 0.50);
        let fresh_p95 = percentile(&fresh_times, 0.95);
        let fresh_p99 = percentile(&fresh_times, 0.99);
        let prec_rep_med = percentile(&prec_reparse_times, 0.50);
        let prec_rep_p95 = percentile(&prec_reparse_times, 0.95);
        let prec_rep_p99 = percentile(&prec_reparse_times, 0.99);
        let cache_med = percentile(&cache_build_times, 0.50);
        let cache_p95 = percentile(&cache_build_times, 0.95);
        let prec_tot_med = percentile(&prec_total_times, 0.50);
        let prec_tot_p95 = percentile(&prec_total_times, 0.95);
        let prec_tot_p99 = percentile(&prec_total_times, 0.99);

        println!(
            "  applied {} edits, skipped {} that broke parse",
            applied, skipped_invalid
        );
        println!(
            "  fresh:                       median {:>5} µs   p95 {:>5} µs   p99 {:>5} µs",
            fresh_med.as_micros(),
            fresh_p95.as_micros(),
            fresh_p99.as_micros()
        );
        println!(
            "  prec_bounded reparse only:   median {:>5} µs   p95 {:>5} µs   p99 {:>5} µs",
            prec_rep_med.as_micros(),
            prec_rep_p95.as_micros(),
            prec_rep_p99.as_micros()
        );
        println!(
            "  cache rebuild (between):     median {:>5} µs   p95 {:>5} µs",
            cache_med.as_micros(),
            cache_p95.as_micros(),
        );
        println!(
            "  prec_bounded total CPU:      median {:>5} µs   p95 {:>5} µs   p99 {:>5} µs",
            prec_tot_med.as_micros(),
            prec_tot_p95.as_micros(),
            prec_tot_p99.as_micros()
        );
        println!(
            "  speedup (reparse only):      median {:.2}×        p95 {:.2}×      p99 {:.2}×",
            fresh_med.as_micros() as f64 / prec_rep_med.as_micros().max(1) as f64,
            fresh_p95.as_micros() as f64 / prec_rep_p95.as_micros().max(1) as f64,
            fresh_p99.as_micros() as f64 / prec_rep_p99.as_micros().max(1) as f64,
        );
        println!(
            "  speedup (total CPU):         median {:.2}×        p95 {:.2}×      p99 {:.2}×",
            fresh_med.as_micros() as f64 / prec_tot_med.as_micros().max(1) as f64,
            fresh_p95.as_micros() as f64 / prec_tot_p95.as_micros().max(1) as f64,
            fresh_p99.as_micros() as f64 / prec_tot_p99.as_micros().max(1) as f64,
        );
    }

    let _ = Node::atom; // keep import used
}
