# Edit-Incremental Pratt Parsing

This repo is an artifact for the paper [***A Menhir for Edit-Incremental Parsing: Sound, Verified
Reuse and Cost-Optimal Recovery for Pratt Parsers***](paper.pdf) (Jeff Pickhardt and
Omniscience Research Agent, 2026), which you can read in this repo directly.

A hand-written Pratt (operator-precedence) parser can be made
**edit-incremental** — reusing untouched subtrees across edits — by reading the
reuse key directly off Pratt's structure: a two-sided *precedence band* the
parser already computes. The predicate is small enough to **machine-verify** and
**generate**: one grammar spec yields a parser, a soundness oracle, and a
machine-checked certificate. The same precedence-bounded region localizes
**cost-optimal error recovery** with a composition guarantee.

This is for the **operator-expression fragment** specifically (concretely: the
hand-written Pratt cores that rust-analyzer, Roslyn, Swift, TypeScript, and V8
ship). It does not subsume LR/GLR generality (see *Positioning* below).

## What the paper argues

Production compilers parse by hand — recursive descent with a Pratt expression
core — because readability, error reporting, and maintainability matter more to
them than the asymptotic generality of LR/GLR. But their tooling has to reparse
on every keystroke, and the existing ways to make that incremental each carry a
structural cost:

- **Reparse from scratch** — simple and always correct, but linear in file size,
  and it discards AST identity across edits, defeating every downstream cache.
- **A separate incremental parser** (tree-sitter, Lezer; the Wagner/Diekmann GLR
  lineage) — deployed and effective, but maintains a *second* grammar and a
  *second* tree that is not the compiler's, so semantic analysis either falls
  back to the from-scratch parser or maintains a fragile cross-grammar mapping.
- **A Roslyn-style reuse predicate** (Roslyn, SwiftSyntax, TypeScript) — makes the
  *hand-written* parser itself incremental via a span+lookahead check. Sound in
  production, but by hand: it rests on a battery of guards plus extensive testing,
  not a theorem, and the bare predicate *without* the guards is silently wrong on
  ~1–2 % of adversarial edits.

**Our claim:** a Pratt parser already computes the reuse key. `parse_expr(min_prec)`
consumes a prefix and then absorbs operators while their binding power exceeds
`min_prec`, so every returned subtree is characterized by two integers — the
minimum binding power on its spine (`m_spine`, the upper bound) and the binding
power of the token that ended it (`stop_lbp`, the lower bound). A cached subtree
is reusable iff that **two-sided precedence band** contains the edit's new
`min_prec`, its text is unchanged, and its tokenization boundary is stable. Two
integer comparisons and a boundary check — and it is **sound by construction**
from Pratt's right-boundary property, not by a guard battery. Where Roslyn rests
on guards and testing, we prove it.

This puts the hand-written Pratt parser on the footing incremental LR already
has from Menhir/Bour — hence *"a Menhir for edit-incremental parsing."* The four
contributions, each backed by something in this repo:

1. **A precedence-bounded reuse predicate** (the *Top-Down Locality Principle*),
   sound by construction.
2. **A formalism + a per-grammar certificate** — the predicate is small enough to
   *machine-verify*, bounded via Kani and unbounded via Creusot.
3. **A generator** — a one-page grammar spec yields the parser, a soundness
   oracle, and the certificate; demonstrated across four grammars. Reuse composes
   from expressions through a *generated* statement host into one tree.
4. **Cost-optimal error recovery** on the same precedence-bounded region —
   localized repair that is *locally optimal = globally optimal* when the error is
   locally contained, closing a gap Diekmann named — plus **identity-keyed
   incremental semantics** that lifts parse-level soundness one layer
   (identity alone is unsound under a binding change; tracking the context a value
   reads restores it).

## Repository map

The repo is the three pieces that back those claims. Each folder has its own
README with the full detail:

| Directory | What it is | Run it |
|---|---|---|
| [`incremental-pratt-gen/`](incremental-pratt-gen/README.md) | **The generator** (contributions 1–3). Grammar spec (TOML) → a standalone Rust crate: from-scratch parser, edit-incremental parser, generated statement front-end, incremental lexing stack, soundness oracle, and a Kani/Creusot certificate. Demonstrated on four grammars (`calc`, `c_expr`, `assoc_stress`, `stmt_lang`). | `make smoke` · `make all` · `make verify` |
| [`reference-impl/`](reference-impl/README.md) | **Hand-written reference implementation** (contributions 1 & 4). The recovery scheme (localized cost-optimal repair) and identity-keyed incremental semantics, with the full benchmark + soundness corpus. | `cargo test` |
| [`verification/`](verification/README.md) | **The certificate** (contribution 2). The reuse predicate verified two ways — Kani (bounded, bit-precise) and Creusot/Why3/SMT (unbounded over `Seq<u8>`) — plus the recovery cost-monoid harnesses. | `cargo kani` · `cargo creusot` |

### [`incremental-pratt-gen/`](incremental-pratt-gen/README.md) — the generator

The headline artifact: a proof-of-concept *"Menhir for edit-incremental parsing."*
It reads a one-page grammar `.toml` and emits a standalone Rust crate containing a
from-scratch Pratt parser, the edit-incremental parser with precedence-bounded
reuse, a soundness oracle (property test: incremental reparse ≡ fresh parse), and
the per-grammar Kani/Creusot certificate. The point is not one fast parser but
that *all of it is derived automatically from the grammar, for more than one
grammar, from a single codegen path* — only three modules end up grammar-specific
(lexer, operator table, grammar text); the parsing engine, cache, and reuse
predicate are byte-identical across every grammar (`diff out/calc/src/pratt_core.rs
out/c_expr/src/pratt_core.rs` → no output). It also carries the layers the papers
describe as future work, demonstrated end-to-end: demand-driven lexing, a
persistent spliceable token store, relex-to-resynchronization, a rope-backed
incremental document, and a generated **statement/declaration host** that composes
expression-level and statement-level reuse into a single tree. Run `make smoke`
(<60s) for the quick tour; see its [README](incremental-pratt-gen/README.md) for
the claims↔target table and reference benchmark numbers (9–15× reparse speedup at
high reuse).

### [`reference-impl/`](reference-impl/README.md) — hand-written reference implementation

The hand-tuned Rust POC the generator's runtime distills from: the full operator
table (ternary, prefix/postfix, 8 precedence classes), the `ReuseCache`, the
recovery scheme, and the empirical story. This is where the **benchmarks** and the
**~62,000-trial soundness corpus** live — including the head-to-head showing
precedence-bounded reuse *strictly dominates* the sound Roslyn-style comparator
(same correctness, higher reuse, ~25× faster), and the balanced-tree builder that
removes the Diekmann long-chain reparse pathology (constant speedup in chain
length). It also documents the three real predicate bugs proptest caught during
development, each now part of the formal predicate statement. See its
[README](reference-impl/README.md) for the full benchmark tables.

### [`verification/`](verification/README.md) — the certificate

The machine-checked proof that the reuse predicate equals its declarative spec,
verified two complementary ways over the *same* source file and predicate body:
**Tier B (Kani/CBMC)** — bit-precise and bounded, exhaustive over symbolic source
pairs up to 32 bytes/side, plus no-panic/no-overflow/in-bounds — and **Tier A
(Creusot/Why3/SMT)** — symbolic and *unbounded*, the same four-part predicate
proved for `Seq<u8>` of any length, discharged automatically by Z3/Alt-Ergo. Six
predicate harnesses plus six harnesses for the min-plus cost monoid that Theorem
4.2 (recovery composition) rests on. The README is candid about what is *not*
verified at either tier and why (parser-symbolic harnesses exceed CBMC
tractability; the predicate quantifies over a strict superset of parser outputs,
so verifying it subsumes verifying it on parser-produced trees). It also has the
pinned-toolchain install for the Creusot tier — see its
[README](verification/README.md) (Tier A).

## Quick start

```sh
# The generator: generate the four parser crates, build them, run a short oracle pass (<60s)
cd incremental-pratt-gen && make smoke

# Full pipeline: generate + property tests + verification + benchmark
make all

# The reuse-predicate certificate (Kani; per generated grammar)
make verify

# The reference implementation's tests (recovery, semantics)
cd ../reference-impl && cargo test

# The standalone two-tier certificate
cd ../verification && cargo kani        # bounded (Kani)
cargo creusot                            # unbounded (Creusot; see verification/README.md for the toolchain)
```

## Claims ↔ evidence

| Paper claim | Where | Evidence |
|---|---|---|
| Precedence-band reuse predicate, sound by construction | `incremental-pratt-gen` | soundness oracle (`make proptest`): incremental reparse == fresh parse over ~400k random edits across four grammars |
| Predicate is machine-checkable, per grammar | `verification`, `incremental-pratt-gen` | Kani 2/2 per generated grammar; standalone certificate = 12 harnesses (6 predicate + 6 recovery monoid); Creusot `Proved` on single-byte grammars |
| Reuse composes into one tree through a *generated* statement front-end, recursively through nested blocks | `incremental-pratt-gen` | `host_recursive_reuse_demo`, `host_chain_matches_fresh`, `host_sample_roundtrip` (incl. the custom `stmt_lang` grammar) |
| Cost-optimal recovery, locally-optimal = globally-optimal for a sole-error locus | `reference-impl`, `verification` | recovery test corpus + the cost-monoid Kani harnesses (the algebra Theorem 4.2 rests on) |

## Requirements

- **Rust** (stable) for the generator and reference implementation.
- **[Kani](https://github.com/model-checking/kani)** (`cargo install --locked kani-verifier && cargo kani setup`) for the bounded certificate.
- **[Creusot](https://github.com/creusot-rs/creusot)** + Why3 + an SMT solver for the unbounded tier — see [`verification/README.md`](verification/README.md) for the pinned-toolchain install.

## Positioning (what this is *not*)

This is the **top-down / Pratt** counterpart to incremental LR (Menhir/Bour), for
the setting where there is no automaton or parser state to key reuse on. We
concede the converse plainly:

- **LR/GLR are more general** — ambiguous grammars and non-LL host constructs lie
  beyond operator-precedence.
- The **statement-layer** reuse here is the standard **reparseable-element**
  mechanism (as in IntelliJ/Roslyn/SwiftSyntax/tree-sitter), generated and
  integrated — *not* the sound-by-construction precedence band, which is specific
  to the expression layer. A by-construction certificate for the statement layer
  would be incremental LL/LR, deliberately out of scope.
- Related and prior work we build on or sit beside: tree-sitter / Lezer and the
  Wagner–Diekmann GLR lineage; Merlin/Menhir (Bour); Roslyn's `Blender`; CPCT+
  (Diekmann & Tratt); and Ciaran Lawlor's functional `incremental-parser`.

## License

[MIT](LICENSE) © 2026 Jeff Pickhardt.

## Citation

The full paper is in this repo: [`paper.pdf`](paper.pdf).

> Jeff Pickhardt and Omniscience Research Agent. *A Menhir for Edit-Incremental
> Parsing: Sound, Verified Reuse and Cost-Optimal Recovery for Pratt Parsers.* 2026.
