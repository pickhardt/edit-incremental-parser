# verification: Two-Layer Verified Predicate for Edit-Incremental Pratt Parsing

Implements the calculator-grammar four-part reuse predicate and the min-plus cost monoid behind cost-optimal error recovery, for the accompanying paper *A Menhir for Edit-Incremental Parsing: Sound, Verified Reuse and Cost-Optimal Recovery for Pratt Parsers*.

- **Tier B (bit-precise, bounded):** twelve Kani harnesses (§8) — six for the four-part reuse predicate (exhaustive over symbolic source pairs up to 32 bytes per side) and six for the recovery cost monoid.
- **Tier A (symbolic, unbounded):** Creusot/Why3/SMT contracts on the reuse predicate (§8) — proved automatically for `Seq<u8>` of any length.

Both layers verify the **same source file** (`src/incremental.rs`) and the **same predicate body**. Tier A is the strict upgrade; Tier B remains valuable as a bit-precise check at the CBMC level with a different, complementary trust chain.

## What's verified — Tier B (Kani)

**Reuse predicate — six harnesses**, each verifying a specific theorem or property:

| Harness | Property | Source size | CBMC checks | Verification time |
|---|---|---|---|---|
| 6.4a | Toolchain smoke test (tautology) | n/a | 4 | 0.02 s |
| 6.4c | Predicate well-formedness (no panic) | 4 bytes | 161 | 0.38 s |
| 6.4d | Post-condition: if `REUSE` true → all 4 conditions hold | 6 bytes | 173 | 2.28 s |
| 6.4e | Post-condition at scale (16 bytes) | 16 bytes | 173 | 5.27 s |
| 6.4g | Post-condition at scale (32 bytes) | 32 bytes | 173 | 11.19 s |
| 6.4f | Negative case: if `REUSE` false → at least one condition fails | 8 bytes | 167 | 2.44 s |

Subtotal: **6 harnesses, 851 CBMC checks, ~22 seconds aggregate verification time on a 2024 laptop** (default-unwind raised to 33 to bound the `next_token_lbp` whitespace-skipping loop at larger source sizes).

**Recovery cost monoid — six harnesses** verifying the min-plus tropical algebra that the recovery composition theorem (Theorem 4.2) rests on. These verify the *algebra*; the cost-optimal repair search and the sole-error/skeleton decision are parser-dependent and are **not** Kani-verified (they live in `reference-impl`).

| Harness | Property |
|---|---|
| `recovery_add_associative` | `add` is associative — costs combine across nested regions in any grouping |
| `recovery_add_commutative_identity` | `add` is commutative with identity `0` (the monoid unit) |
| `recovery_add_monotone_bounded` | the Theorem 4.2 inequality `add(inside, outside) ≥ inside`, plus overflow-safety |
| `recovery_min_semilattice` | `min` is a semilattice (choosing the cheaper repair) |
| `recovery_distributive` | distributivity of `add` over `min` (pushing region costs through choices) |
| `recovery_total_consistent` | `total` accumulates unit repair costs correctly (n repairs cost n) |

All six: `VERIFICATION:- SUCCESSFUL`.

## What's verified — Tier A (Creusot)

Creusot contracts on `reuse_predicate` (and the operational primitives it calls) verify the same four-part predicate of Definition 3.3 over **arbitrarily long** `Seq<u8>` sources:

```rust
#[requires(edit.start@ + edit.removed@ <= usize::MAX@)]
#[requires(t_old.end@ + edit.added@ <= usize::MAX@)]
#[requires(t_old.start@ <= t_old.end@)]
#[requires(t_old.end@ + edit.added@ - edit.removed@ <= new_src@.len())]
#[requires(edit.start@ + edit.added@ <= new_src@.len())]
#[ensures(result == (
    cond_precedence_band(*t_old, m_new)
    && cond_disjoint_region(*t_old, *edit)
    && cond_boundaries_match(*t_old, *edit, old_src@, new_src@)
    && cond_next_token_lbp(*t_old, *edit, new_src@)
))]
pub fn reuse_predicate(...) -> bool { /* body unchanged */ }
```

The four `#[logic(open)]` helpers (`cond_precedence_band`, `cond_disjoint_region`, `cond_boundaries_match`, `cond_next_token_lbp`) encode the four conditions of Definition 3.3 verbatim. `cond_next_token_lbp` depends on `next_token_lbp_logic`, a Pearlite recursive function in `lexer.rs` that skips whitespace and returns the lbp of the next non-whitespace byte (termination measure: `src.len() - pos`). `Edit::translate_to_new`, `Edit::old_range`, and `next_token_lbp` carry their own contracts and are verified against them, so the verification chain extends down through every operational primitive `reuse_predicate` calls.

| Property verified | Tactic | Solver | Time |
|---|---|---|---|
| `reuse_predicate` ensures (full iff over `Seq<u8>`, 4 conditions) | `compute_specified → split_vc` | Alt-Ergo + Z3 | ~0.5 s solver |
| `Edit::translate_to_new` ensures | direct | Alt-Ergo | 0.011 s |
| `Edit::old_range` ensures | direct | Alt-Ergo | 0.010 s |
| `next_token_lbp` ensures (matches logic version) | direct | Alt-Ergo | 0.011 s |
| 16 trivial derive-generated VCs | direct | Alt-Ergo | < 0.01 s each |

Total: **24 verified `.coma` files, all goals discharged automatically, ~2–3 s wall-clock (Why3 orchestration included).** No manual `proof_assert` hints were needed. (The recovery cost-monoid module compiles under Creusot but carries no contracts — it is verified by the Kani harnesses, Tier B — so its files discharge as trivial VCs.)

## What's not verified (at either layer, and why)

**Parser-based verification** is out of scope at both layers:

- **Kani (Tier B):** parser-symbolic harnesses exceed CBMC tractability for any meaningful input size. Empirically, a 2-byte parser-based harness did not complete in 10 minutes; a 3-byte one did not complete in 5 minutes.
- **Creusot (Tier A):** would require loop invariants on `parse_expr`'s top loop, a decreases measure on its recursion, and well-formed-output post-conditions on emitted `Node`s. A multi-week annotation project, not the predicate-only upgrade demonstrated here.

This is **not a verification gap**: the predicate-side harnesses (Tier B) and the predicate contract (Tier A) both quantify over `Node` values with arbitrary `start`/`end`/`m_spine`/`stop_lbp`, which is a *strict superset* of parser outputs. Soundness (Theorem 3.9) reduces via Lemma 3.4 (deterministic-Pratt-output-equality) to the predicate's correctness, which is what both tiers establish.

The recovery scheme of §6 is present here as its **cost algebra**: the min-plus tropical monoid in `recovery.rs` is verified by the six recovery harnesses above (Tier B). The cost-optimal repair *search* and the sole-error/skeleton decision are parser-dependent and remain unverified at both tiers — only the algebra that Theorem 4.2 rests on is mechanized here. (The full recovery implementation lives in `../reference-impl`.)

## Running the verification

### Tier B (Kani)

Requires Rust 1.95+ and Kani 0.67+.

```sh
# Install Kani (one-time)
cargo install --locked kani-verifier
cargo kani setup

# Verify all six harnesses
cargo kani

# Verify a single harness
cargo kani --harness harness_6_4d_predicate_post_condition

# Run the unit tests (validates the implementation independently)
cargo test
```

Expected output: `VERIFICATION:- SUCCESSFUL` and `0 of N failed` per harness.

### Tier A (Creusot)

Requires Rust nightly-2026-04-21 (Creusot's pinned toolchain), opam, Why3 (git build), and an SMT solver (Z3 4.15+ and/or Alt-Ergo 2.6+).

This artifact's `Cargo.toml` pins `creusot-std` to the same upstream commit (`34e1aecf`) that `cargo-creusot` was built from. The pair must match: Creusot's macro-generated Coma output is coupled to the verifier binary's expectations. If you install a different upstream Creusot commit, re-pin `creusot-std` in `Cargo.toml` to match.

**Install Creusot (one-time):**

```sh
# Install dependencies (macOS via Homebrew shown; adjust for your OS)
brew install opam z3 alt-ergo

# Clone Creusot at the pinned commit
git clone https://github.com/creusot-rs/creusot
cd creusot
git checkout 34e1aecf46060e9479b803a85476022eeeed4728

# Install the Creusot toolchain
./INSTALL --external z3 --external alt-ergo
```

**One macOS gotcha:** Creusot's `creusot-deps.opam` requires `why3-ide` (GTK3 dependency) by default, which pulls in `gtk+3` and `gtksourceview3` via Homebrew. If you don't want the GUI, edit `creusot-deps.opam` before running `./INSTALL` and drop the `"why3-ide"` line and its pin (the `!?in-creusot-ci` filter exists for exactly this reason).

The install takes ~10–15 minutes (OCaml 5.3.0 + Why3 + cargo-creusot build).

**Reproduce the verification:**

```sh
# From verification/
cargo creusot
```

Expected output: `Proved (24 files) ✔`. First run is slow (~30 s; pulls `creusot-std` from git and compiles its proc-macros); subsequent runs are ~1.5 s.

## Project structure

```
src/
├── lib.rs              -- crate root, module declarations
├── lexer.rs            -- byte-level lexer; Token has DeepModel derive under cfg(creusot)
├── parser.rs           -- Pratt parser, arena-based Node; #[trusted] for Creusot
├── incremental.rs      -- Edit + four-part reuse predicate with Creusot contracts
├── recovery.rs         -- min-plus cost monoid + Repair (verified by the recovery harnesses)
└── harnesses.rs        -- twelve Kani harnesses (6 predicate + 6 recovery), cfg(kani)-gated

why3find.json           -- Creusot prover config (timeouts, tactic order)
```

## Implementation notes (relevant for paper §3 and §8)

- **Arena-based parse tree.** Nodes are `Copy` records in a fixed-size array `[Node; MAX_NODES]`, referencing each other by `NodeId` (index). Recursive `Box<Node>` produced unbounded `drop_in_place` recursion that CBMC could not unwind. The arena representation eliminates all heap interaction.
- **Bounded recursion and loops.** `parse_expr` takes an explicit `depth` parameter capped at `MAX_PARSE_DEPTH`; the top-loop iterations are bounded by `MAX_PARSE_ITER`. Both bounds are conditional: tight for Kani builds (`#[cfg(kani)]`), generous for production. The same source verifies at both bounds.
- **Predicate is the verification target, not the parser.** Hand-constructed symbolic `Node`s with arbitrary `start`/`end`/`m_spine`/`stop_lbp` are a strict superset of any parser output, so verifying the predicate at scale subsumes verifying it on parser-produced trees. This holds at both Tier A and Tier B.
- **Creusot derive interactions.** Under `cfg(creusot)`, `Token` uses Creusot's `PartialEq` derive (imported from `creusot_std::std::cmp::PartialEq`) and a manual `DeepModel` derive, both gated on `cfg(creusot)`. `Node`, `NodeKind`, and `Repair` have their `PartialEq`/`Eq` derives `cfg_attr`-gated to `not(creusot)` since the predicate doesn't need them and the derive expansion under Creusot would otherwise require parallel `DeepModel` impls.
- **Trusted scope.** Parser internals, the lexer, the recovery search, and `reparse` are marked `#[trusted]` at the Creusot layer — they're not the verification target. The predicate's verification chain does not call any trusted function; it only calls `Edit` methods (verified) and standard slice/Option methods (extern-spec'd by `creusot-std`).

## Reproducibility

Kani verification times are from a 2024 Apple Silicon laptop; CBMC's `--unwind 16` default. Expect ±2x variation on different hardware. Kani harnesses are deterministic (CBMC exhaustively explores the bounded state space).

Creusot's solver times are also reasonably stable; `cargo creusot`'s wall-clock includes Why3 orchestration overhead beyond the raw SMT time. The pinned `creusot-std` git revision (`34e1aecf`) is the load-bearing reproducibility anchor — if the upstream commit moves and your `cargo-creusot` binary was built from a different one, expect macro-version errors.

## Citation

If you use this artifact, please cite the accompanying paper:

> *A Menhir for Edit-Incremental Parsing: Sound, Verified Reuse and Cost-Optimal Recovery for Pratt Parsers.* Jeff Pickhardt, 2026.
