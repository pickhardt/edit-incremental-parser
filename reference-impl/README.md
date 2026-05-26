# Incremental Pratt POC

Rust proof-of-concept for **precedence-bounded subtree reuse** in
incremental Pratt parsing.

## What's here

- `src/lexer.rs` — character-level lexer with byte spans
- `src/op.rs` — operator binding-power tables (lbp 5–70 across 8
  precedence classes; left- and right-associative infix; prefix `-`/`!`;
  ternary `?:`)
- `src/ast.rs` — Arc-shared `Node` with width-based positions (Roslyn
  red/green tree idea). Children are `Arc<Node>`; reuse is `Arc::clone`.
- `src/parser.rs` — vanilla Pratt parser, builds `Arc<Node>`
- `src/incremental.rs` — precedence-bounded incremental parser + the
  `ReuseCache` (built once per loaded tree, reused across edits)
- `src/recovery.rs` — localized **cost-optimal error recovery**: a
  min-plus cost monoid and a repair search confined to the
  precedence-bounded region around the break (paper §6 / Theorem 4.2)
- `src/semantics.rs`, `src/semantics_ctx.rs` — **identity-keyed
  incremental semantics**: a downstream pass memoized on node identity
  that reuses results exactly where the parser reused subtrees; the
  `_ctx` variant tracks the context a value reads (sound under a binding
  change, where identity alone is not — paper §7). *Gated behind the
  `node_id` feature.*
- `src/chain_wb.rs` — node-span side table + the path-copy splice fast
  path for associativity-conflict chains (the `chain_splice` feature)
- `src/span_lookahead.rs` — naive span + N-token-lookahead comparator
  (textbook predicate, **may be unsound**)
- `src/roslyn_style.rs` — sound Roslyn-style comparator (M_floor +
  lookahead). Strictly more conservative than precedence-bounded.
- `src/edit.rs` — edit representation + old↔new byte mapping
- `src/bin/bench.rs`, `src/bin/realistic_bench.rs` — empirical comparison
  harnesses (`src/bench_support.rs` is their shared scaffolding)
- `src/bin/find_counterexample.rs` — finds concrete cases where span+
  lookahead diverges from the fresh parse
- `tests/equivalence.rs`, `tests/wagner_fig10.rs`, `tests/stress.rs` —
  the reuse soundness suite
- `tests/recovery_theorem.rs`, `tests/recovery_incremental.rs` — the
  recovery corpus (locally-optimal = globally-optimal for a sole-error
  locus; recovery under incremental reparse)
- `tests/semantics.rs`, `tests/semantics_ctx.rs` — the incremental-semantics
  validation (soundness + locality); run with `--features node_id`

## The reuse predicate (final form)

A cached subtree `T` is reusable as the result of `parse_expr(M_new)`
at the corresponding byte position iff **all three** conditions hold:

1. **Precedence band**: `T.stop_lbp ≤ M_new < T.m_spine`
   * Upper: every operator on T's top-loop spine still binds tightly
     enough at the new floor.
   * Lower: the token that ended T's parse would also end the new
     parse — i.e., the new parser doesn't extend past T.
2. **Text region**: T's old byte span is entirely outside the edit.
3. **Tokenization boundary**: the bytes immediately before and after
   T's span agree in old and new sources (so the lexer produces the
   same token boundaries).

Reuse is `Arc::clone` of a cached subtree — O(1) refcount bump
regardless of subtree size.

(The paper's formalization and the `../verification` certificate state
this as a **four**-part predicate: the lower bound of condition 1 is
split out as a separate *next-token-lbp* condition, so that conditions 3
and 4 are distinct for multi-byte lexers. The three-condition form here
is the equivalent hand-POC presentation for the single-byte calculator
grammar, where the two coincide.)

## Empirical results

Per-edit reparse time (cache amortized across edits, as in a real LSP):

| Size | fresh | **prec_bounded** | speedup | reuse | incorrect |
|------|------:|----------------:|--------:|------:|----------:|
| 200B  |   6 µs |   **4 µs** |  1.5× | 86.4% | **0/89** |
| 1KB   |  22 µs |   **8 µs** | **2.8×** | 96.2% | **0/93** |
| 5KB   | 111 µs |  **46 µs** | **2.4×** | 99.0% | **0/92** |
| 20KB  | 315 µs | **133 µs** | **2.4×** | 99.5% | **0/86** |
| 80KB  | 330 µs | **139 µs** | **2.4×** | 99.6% | **0/82** |

### Realistic edit-sequence benchmark

500 sequential cursor-clustered, structurally-meaningful edits on a
single source, with cache maintained between edits (LSP-shaped
workload). Correctness asserted on every applied edit.

| Source | fresh med | **prec_bounded med** | speedup | p95 | p99 |
|--------|----------:|--------------------:|--------:|----:|----:|
| 1KB    | 29 µs     | **8 µs**            | **3.6×** | 3.7× | 3.5× |
| 5KB    | 123 µs    | **40 µs**           | **3.1×** | 2.6× | 2.9× |
| 20KB   | 161 µs    | **54 µs**           | **3.0×** | 2.5× | 2.5× |

Bounded tail latency — p99 still 2.5-3.5× faster than fresh, so the
speedup isn't a median artifact. Higher than the adversarial-random
speedup because cursor-clustered edits leave more of the tree
provably stable.

Run with `cargo run --release --bin realistic_bench`.

### Long associative chains (Diekmann §2.9 pathology shape)

Source shape: `(operand) + (operand) + ... + (operand)` where each
operand is a non-trivial subexpression. Edit: change one byte inside
one middle operand. Balanced-tree builder for `+`, `*`, `&&`, `||`
gives O(log n) AST depth from the same Pratt grammar.

| Chain len | Bytes | fresh | **prec_bounded** | speedup | reuse |
|-----------|------:|------:|----------------:|--------:|------:|
|  100  |  1.8KB |  38 µs |   **13 µs** | **2.9×** | 88.2% |
|  500  |  9.0KB | 192 µs |   **65 µs** | **3.0×** | 88.8% |
| 1000  | 18.0KB | 384 µs |  **127 µs** | **3.0×** | 88.8% |
| 2500  | 45.0KB | 960 µs |  **320 µs** | **3.0×** | 88.9% |

Speedup is constant in chain length because balanced shape bounds
per-edit reuse cost regardless of total operand count.

### Comparators at 20KB random expressions

| Strategy | Time | Reuse | Incorrect |
|----------|-----:|------:|----------:|
| fresh             |  315 µs |    0% |   0/86 |
| **prec_bounded**  | **133 µs** | 99.5% | **0/86** |
| span_la0 (unsound) | 491 µs | 99.7% | 1/86 |
| span_la1 (unsound) | 1504 µs | 99.7% | 0/86 |
| roslyn_style (sound) | 3279 µs | 99.4% | 0/86 |

**Precedence-bounded strictly dominates the sound Roslyn-style
equivalent**: same correctness, slightly higher reuse, 25× faster.

## Soundness evidence

| Category | Cases | Result |
|---|---:|---|
| Unit / hand-written | 30 | ✅ |
| Equivalence proptest (main) | 5,000 | ✅ |
| Right-assoc chain stress | 3,000 | ✅ |
| Ternary chain stress | 3,000 | ✅ |
| Multi-edit sequence stress | 1,500 | ✅ |
| Long-chain stress (incl. 1000-op) | 4 | ✅ |
| Benchmark trials | ~700 | ✅ |
| One-off 50k proptest run | 50,000 | ✅ |
| **Total** | **~62,000 trials** | **0 incorrect** |

Three real predicate bugs were caught by proptest during development:
the `stop_lbp` lower bound on subtrees, the boundary-byte check, and
the `stop_lbp` inheritance on interior balanced-builder nodes. All
three are now part of the formal predicate statement.

## Running

```bash
cargo test                     # 76 reuse + recovery tests (~13s; recovery_theorem dominates)
cargo test --features node_id  # +17 identity-keyed semantics tests (93 total)
cargo run --release --bin bench
cargo run --release --bin realistic_bench
cargo run --release --bin find_counterexample
```

The `node_id` feature is off by default so the benchmarked hot parse
path pays no id-minting cost; it (and `chain_splice`, which implies it)
gate the incremental-semantics modules and their tests.

For deeper proptest, bump `cases: 5000` in `tests/equivalence.rs`
to 50_000 and re-run.

## What this is not

- A full incremental parser for a real language — pure expressions only.
  Whole-file incremental requires embedding inside a recursive-descent
  host grammar.
- A multi-edit batch interface — the parser accepts one contiguous edit
  at a time. Stress tests confirm sequential edits work correctly.
- End-to-end verified — the *reuse predicate* and the recovery *cost
  monoid* are machine-checked (see `../verification`), but the parser, the
  recovery search, and the semantics pass themselves are not. That is the
  open research problem, stated as such in the papers.

Error recovery **is** here (`src/recovery.rs`): localized cost-optimal
repair confined to the precedence-bounded region around the break, which
is locally-optimal = globally-optimal when the error is the sole locus
(paper §6, closing the gap Diekmann names). Earlier drafts of this POC
bailed on the first error; that is no longer the case.
