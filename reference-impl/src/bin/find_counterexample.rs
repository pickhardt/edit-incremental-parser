//! Find concrete cases where span+lookahead produces a wrong parse but
//! precedence-bounded is correct. These are the rhetorical anchors for
//! the paper.

use incremental_pratt_poc::{
    bench_support::{gen_atom, gen_binop, Lcg},
    incremental_parse, parse, span_lookahead_parse, Edit,
};

fn gen_expr(target_bytes: usize, mut rng: Lcg) -> String {
    fn gen(depth: u32, budget: &mut usize, rng: &mut Lcg) -> String {
        if *budget < 4 || depth > 12 {
            return gen_atom(rng);
        }
        let choice = rng.range(10);
        *budget = budget.saturating_sub(3);
        match choice {
            0..=4 => {
                let l = gen(depth + 1, budget, rng);
                let op = gen_binop(rng);
                let r = gen(depth + 1, budget, rng);
                format!("{} {} {}", l, op, r)
            }
            5 => format!("({})", gen(depth + 1, budget, rng)),
            6 => format!("-{}", gen(depth + 1, budget, rng)),
            7 => format!("!{}", gen(depth + 1, budget, rng)),
            _ => {
                let c = gen(depth + 1, budget, rng);
                let t = gen(depth + 1, budget, rng);
                let e = gen(depth + 1, budget, rng);
                format!("({} ? {} : {})", c, t, e)
            }
        }
    }
    let mut budget = target_bytes;
    gen(0, &mut budget, &mut rng)
}

fn gen_edit(src: &str, mut rng: Lcg) -> Edit {
    let len = src.len() as u32;
    if len == 0 {
        return Edit { start: 0, end: 0, replacement: "a".to_string() };
    }
    let pos = rng.range(len as usize) as u32;
    let kind = rng.range(5);
    match kind {
        0 => {
            let c = (b'0' + rng.range(10) as u8) as char;
            Edit { start: pos, end: (pos + 1).min(len), replacement: c.to_string() }
        }
        1 => {
            let c = match rng.range(4) { 0 => 'a', 1 => 'b', 2 => 'x', _ => 'y' };
            Edit { start: pos, end: (pos + 1).min(len), replacement: c.to_string() }
        }
        2 => Edit { start: pos, end: pos, replacement: " ".to_string() },
        3 => {
            let c = (b'0' + rng.range(10) as u8) as char;
            Edit { start: pos, end: pos, replacement: c.to_string() }
        }
        _ => {
            let c = match rng.range(7) {
                0 => '+', 1 => '-', 2 => '*', 3 => '/',
                4 => '<', 5 => '>', _ => '%',
            };
            Edit { start: pos, end: (pos + 1).min(len), replacement: c.to_string() }
        }
    }
}

fn main() {
    let target_count = 5;
    let mut found = 0;
    let mut seed: u64 = 0xc0ffee;

    for _ in 0..200_000 {
        if found >= target_count {
            break;
        }
        seed = seed.wrapping_add(0x9E3779B97F4A7C15);
        let src = gen_expr(80, Lcg(seed));
        let edit = gen_edit(&src, Lcg(seed.wrapping_add(1)));

        let old_tree = match parse(&src) {
            Ok(n) => n,
            Err(_) => continue,
        };
        let new_src = edit.apply(&src);
        let fresh = match parse(&new_src) {
            Ok(n) => n,
            Err(_) => continue,
        };

        let (prec, _, _) = match incremental_parse(&old_tree, &src, &edit) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let (span1, _, _) = match span_lookahead_parse(&old_tree, &src, &edit, 1) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let fresh_str = fresh.unparse_normalized();
        let prec_str = prec.unparse_normalized();
        let span_str = span1.unparse_normalized();

        if prec_str == fresh_str && span_str != fresh_str {
            found += 1;
            println!("\n--- counterexample #{} ---", found);
            println!("  old src:        {:?}", src);
            println!("  edit:           bytes [{}..{}) -> {:?}", edit.start, edit.end, edit.replacement);
            println!("  new src:        {:?}", new_src);
            println!("  fresh parse:    {}", fresh_str);
            println!("  prec_bounded:   {}  (matches fresh)", prec_str);
            println!("  span+1 lookahead: {}  (WRONG)", span_str);
        }
    }
    if found == 0 {
        println!("No counterexamples found.");
    }
}

