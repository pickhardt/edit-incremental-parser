//! Shared support code for the benchmark binaries in `src/bin/`.
//!
//! Each binary previously had its own (subtly-divergent) copies of
//! `Lcg`, `gen_atom`, `gen_binop`, etc. They are factored here so
//! benchmark behaviour stays consistent across `bench`,
//! `realistic_bench`, and `find_counterexample`.
//!
//! Binary-specific expression generators (`gen_expr` shapes,
//! `gen_base_source`, edit-sequence simulators) stay in the binaries
//! that own them — their shapes are deliberately specialised.

/// 64-bit linear congruential generator used by all benchmark
/// binaries. Deterministic and seedable for reproducible runs.
#[derive(Clone, Copy)]
pub struct Lcg(pub u64);

impl Lcg {
    pub fn new(seed: u64) -> Self {
        Lcg(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }

    pub fn range(&mut self, n: usize) -> usize {
        (self.next() as usize) % n.max(1)
    }
}

/// Atom-producing helper: picks one of `a`, `x`, `n`, or a small int.
/// Used by every benchmark binary's expression generator.
pub fn gen_atom(rng: &mut Lcg) -> String {
    match rng.range(4) {
        0 => "a".to_string(),
        1 => "x".to_string(),
        2 => format!("{}", rng.range(100)),
        _ => "n".to_string(),
    }
}

/// Binary infix operator picker. 13-way distribution biased slightly
/// toward `+` and `*` (the AssociativityConflict operators that
/// exercise the balanced builder).
pub fn gen_binop(rng: &mut Lcg) -> &'static str {
    match rng.range(13) {
        0 => "+",
        1 => "-",
        2 => "*",
        3 => "/",
        4 => "&&",
        5 => "||",
        6 => "==",
        7 => "!=",
        8 => "<",
        9 => ">",
        10 => "%",
        11 => "+",
        _ => "*",
    }
}

/// Member-access field-name picker. Used for the postfix `.field`
/// form added in the grammar expansion.
pub fn gen_field(rng: &mut Lcg) -> &'static str {
    match rng.range(4) {
        0 => "f",
        1 => "g",
        2 => "field",
        _ => "next",
    }
}
