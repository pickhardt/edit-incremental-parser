# incremental-pratt-gen

A proof-of-concept **"Menhir for edit-incremental parsing."** It reads a
one-page grammar specification and emits a standalone Rust crate containing:

1. a from-scratch Pratt parser,
2. an **edit-incremental** parser with precedence-bounded subtree reuse,
3. a **soundness oracle** (property test: incremental reparse ≡ fresh parse), and
4. a **machine-checked certificate** of the reuse predicate, re-discharged
   per grammar by [Kani](https://model-checking.github.io/kani/).

The point of the PoC is not a single fast parser — the companion papers
already showed that for one hand-written grammar. The point is that all of
the above is **derived automatically from the grammar**, for *more than one*
grammar, from a single code-generation path. That is the only claim the two
papers leave unproven, and it is what this artifact demonstrates.

This implements the operator-expression fragment of the Top-Down Locality
Principle (Pickhardt 2026, papers 1 & 2). It is deliberately scoped (see
[Scope](#scope)); it is not a whole-language parser generator.

## Quick start

```
make smoke         # generate + build + short oracle pass for all grammars (< 60s)
make all           # gen + proptest (50k) + verify (Kani) + bench
make verify-all    # both certificate tiers: Kani (all) + Creusot (single-byte)
```

Requirements: a stable Rust toolchain (`cargo`). `make verify` additionally
needs Kani:

```
cargo install --locked kani-verifier && cargo kani setup
```

`make verify-creusot` needs the Creusot toolchain (opam + Why3 + an SMT
solver + a pinned Rust nightly) — see `verification/README.md` (Tier A) for the
one-time install.

### Docker

`Dockerfile` reproduces the generator + oracle + **Kani** tier + bench in a
container (the Creusot tier stays host-side; see the Dockerfile header):

```
docker build -t ipg . && docker run --rm ipg make all
```

## What gets generated

Run `make gen`, then look in `out/<grammar>/`. Only three modules are
grammar-specific (`src/lexer.rs`, `src/op.rs`, `src/grammar_text.rs`); the
rest of the crate is the shared runtime, **byte-identical across every
grammar**:

```
diff out/calc/src/pratt_core.rs out/c_expr/src/pratt_core.rs   # no output
```

So a reviewer can see exactly what the generator specialises (the operator
tables and lexer) and what it does not (the parsing engine, the cache, the
reuse predicate). Adding a fourth grammar is writing a `.toml` — try copying
`grammars/calc.toml` and changing the operator table.

## Claims ↔ evidence

Every claim in the writeup maps to a `make` target and an expected output.

| Claim | Target | Expected output |
|---|---|---|
| The generator is grammar-parametric: one codegen path, N grammars, identical runtime | `make gen` + the `diff` above | four `out/<g>/` crates; runtime files identical |
| Generated parsers are **sound**: incremental reparse ≡ fresh parse over random + structured edits | `make proptest` | `test result: ok` (2 tests × 50k cases × 4 grammars = 400k) |
| The persistent token store's splice + byte-lookup match a flat reference | `make proptest` | `store_matches_reference` `ok` (4k cases × 4) |
| Incremental relexing equals a full re-tokenize (incl. multi-byte operators) | `make proptest` | `relex_matches_full` `ok` (50k cases × 4) |
| The relex window is O(1) — independent of file size | `make proptest` | `relex_window_is_local` `ok` (window = 2 tokens at 1.2k→19k) |
| The rope-backed incremental document's tokens match a full tokenize | `make proptest` | `document_matches_full` `ok` (rope splice + relex-from-rope) |
| Single-tree two-level reuse: incremental *program* reparse ≡ fresh | `make proptest` | `host_incremental_matches_fresh` `ok` (50k × 4) |
| A keystroke reuses untouched statements + expr subtrees in the edited one | `make proptest` | `host_reuse_demo` `ok` (4/5 stmts + 12 expr subtrees reused) |
| The reuse predicate is **machine-checked (bounded)**, re-discharged per grammar | `make verify` | `VERIFICATION:- SUCCESSFUL` (2 Kani harnesses × 4 grammars) |
| The reuse predicate is **machine-checked (unbounded)** for single-byte grammars | `make verify-creusot` | `Proved (5 files) ✔` (calc, assoc_stress, stmt_lang; c_expr skipped — multi-byte) |
| Edit-incremental reparse is **faster** than from-scratch, at high reuse | `make bench` | speedup > 1, reuse % near 100 |

### Reference numbers (Apple M-series; ratios are the reportable result)

`make bench` on nested sources (a leaf edit reuses an entire sibling subtree).
`lexed %` is the fraction of the file's tokens the incremental reparse
actually lexed — demand-driven lexing (below) means it lexes only what it
visits, not the whole file:

| grammar | bytes | fresh µs | reparse µs | speedup | reuse % | lexed % |
|---|---|---|---|---|---|---|
| calc | 32765 | 1034 | 67 | **15.5×** | 99.9 | 0.2 |
| c_expr | 40956 | 613 | 68 | **9.0×** | 99.9 | 0.2 |
| assoc_stress | 32765 | 1052 | 74 | **14.3×** | 99.9 | 0.2 |
| stmt_lang | 32765 | 1013 | 62 | **16.4×** | 99.9 | 0.2 |

Kani: 2 harnesses per grammar, ~7–21 s each, ≈109–112 checks (including
no-panic / no-overflow / in-bounds), all `SUCCESSFUL` across all four
grammars (`stmt_lang` included).

Absolute times vary with hardware (~10% run-to-run); speedup ratios are
the intended figure. The reparse time is the parser+lexer hot path; the
editor-side buffer update (`apply`, the string copy) is measured separately
since the editor owns the buffer. Flat single-operator chains (the Diekmann
pathology) reuse — and so re-lex — far more than the nested sources benched
here; this is expected and discussed in the papers.

### Demand-driven lexing

The parser does **not** tokenize the whole file per edit. It pulls tokens
on demand from a `Lexer` cursor (`src/cursor.rs`), and on a reuse it
**repositions the cursor past the reused subtree** — so a reused subtree's
interior is never lexed. Lexing work therefore tracks the reparsed region,
not the file size (the `lexed %` column: ~0.2% at 32 KB). This composes
naturally with precedence-bounded reuse, which already jumps the parser past
reused subtrees. (It does not help the low-reuse case — a flat chain still
lexes most of the file — which is where a future relex-to-resynchronization
pass over a persistent token store would add value.)

### Persistent token store (`src/token_store.rs`)

A persistent, spliceable token stream: an implicit treap of tokens with
**relative offsets** (each entry stores its `gap` + `width`, not an absolute
position), so a splice in the middle leaves the suffix untouched and
positions are recomputed by prefix sum on traversal — the red/green-tree
trick applied to tokens. It supports O(log n) `index_at_byte` lookup and
O(log n + k) `splice`, validated against a flat `Vec` reference by a
property test (`store_matches_reference`).

**This is not a parser speedup, and the bench is unchanged by design.** For a
cheap lexer like ours, demand-driven lexing already captures the parser-side
win: reading a token from an O(log n) store is no cheaper than re-lexing it
at O(1), and on low-reuse edits the parser traverses the tokens regardless.
The store earns its keep elsewhere — a persistent full token stream that
stays current across edits without re-lexing the file, for **whole-stream
consumers** (syntax highlighting, semantic tokens, bracket matching) and for
**expensive lexers** where avoiding a re-lex actually saves work. It is the
substrate the relex-to-resynchronization pass (below) updates in
O(edit + resync); the parser keeps using demand-driven lexing.

### Relex-to-resynchronization (`src/relex.rs`)

`relex(store, old_src, new_src, edit)` brings the persistent token store up
to date in place after an edit, **without re-tokenizing the file**. It
re-lexes a window starting at the token before the edit and stops at
**resynchronization** — the first freshly lexed token that matches an old
token (same kind, width, and gap) at its delta-shifted position. From there
the old suffix is byte-identical and is kept (shifted for free by the store's
relative offsets). It returns the window size (tokens re-lexed).

Two properties are property-tested per grammar:

- **`relex_matches_full`** (50k cases): the relexed store equals a full
  `tokenize` of the new source, token-for-token. This holds for the
  multi-byte-operator grammar (`c_expr`) too — the interesting case, where an
  edit can split or merge `&&`, `==`, `<=`, etc.
- **`relex_window_is_local`**: a single-token edit in the middle of a source
  re-lexes a **constant** number of tokens (2) as the file grows 16×
  (1,202 → 19,202 tokens) — O(edit + resync), not O(n).

Soundness rests on the lexer restarting cleanly at each token boundary
(1-byte context, no lexical modes). The worst case is still O(n) — an edit
whose lexical effect runs to end-of-file (e.g. an unterminated construct) —
which is fundamental, not an implementation limit. This is what keeps a
whole-token-stream consumer (highlighting/semantic tokens) current per
keystroke without re-lexing the file; it does not change the parser path.

### Rope-backed incremental document (`src/document.rs`)

`IncrementalDocument` is the capstone: the source text in a **rope**
(`ropey`) plus the token stream in the persistent store, both updated per
edit with **no full re-copy and no full re-tokenize**. `edit(start, end,
repl)` splices the rope in O(log n) and relexes the store in
O(edit + resync) — and, crucially, the relex reads *straight from the rope*
via the `ByteSource` trait, so the new text is never materialized into a
contiguous buffer. `document_matches_full` checks that after an edit the
document's tokens equal a full tokenize of the edited text.

To make this possible the generated `lex_token` is generic over
`ByteSource` (implemented for `&[u8]` — the parser's fast path — and for a
rope view). This is the layer an editor integration would hold to keep
highlighting / semantic tokens live on every keystroke.

**It does not change the parser**, which keeps demand-driven lexing over a
`&[u8]` snapshot; making the *parser* read from the rope (a streaming
rope-lexer) is a further step and is not done here. (ASCII is assumed for
the byte↔char index mapping into the rope, consistent with the rest of the
PoC.)

## The grammar DSL

```toml
name = "calc"
[atoms]        # which atom kinds the lexer produces
int = true
[paren]        # the bracket pair used for grouping
open = "("
close = ")"
[[prefix]]     # unary prefix operators
sym = "-"
rbp = 100
[[postfix]]    # unary postfix operators
sym = "!"
lbp = 90
[[infix]]      # binary infix operators
sym = "+"
lbp = 50
assoc = "left"        # left | right
class = "assoc"       # weak | assoc | strong  (Li–Taura AOPP conflict type)
```

`class` selects the runtime three-way dispatch: `assoc` (associativity
conflict) builds a balanced tree and admits re-association under
`unparse_normalized`; `weak` keeps a strict left/right-leaning fold;
`strong` is reserved for constructs that require escalation (folded to
`weak` here, as in the paper's POC — no strong-conflict construct is in
scope).

## How the certificate works

`out/<g>/src/verify.rs` (shared runtime) states the four-part
precedence-bounded reuse predicate (Definition 3.3) twice: once
operationally (early-return, as the parser evaluates it) and once
declaratively (`spec_band`, `spec_disjoint`, `spec_boundaries`,
`spec_next_token`). The Kani harnesses prove the two agree — and that the
predicate never panics or indexes out of bounds — for **every** bounded
symbolic `(node, m_new, edit, old_src, new_src)` tuple. The only
grammar-specific input is `op::next_token_lbp` (condition 4), which the
generator specialises to each grammar's operator set; `make verify`
re-discharges the proof against that specialised function per grammar.

This is the miniature of paper 2's mechanization: the load-bearing
predicate, verified, regenerated per grammar. It is **not** end-to-end
verification of the parser or of recovery — those are unverified in the
papers too, and are the open research problem, not part of this PoC.

### Two certificate tiers

Mirroring paper 2's two-layer approach, the generator emits both:

- **Bounded (Kani / CBMC), all grammars** — `make verify`. Bit-precise,
  exhaustive over symbolic source pairs up to 6 bytes per side; also proves
  no panic / no overflow / in-bounds. `src/verify.rs`.
- **Unbounded (Creusot / Why3 / SMT), single-byte-operator grammars** —
  `make verify-creusot`. Proves the same predicate equals its declarative
  spec for `Seq<u8>` of *any* length, discharged automatically by Z3 /
  Alt-Ergo. Emitted as a self-contained sibling crate at
  `out/<g>/creusot/` so the heavy Creusot git dependency never touches the
  default build or the Kani tier.

The Creusot tier is restricted to single-byte-operator grammars (calc,
assoc_stress, stmt_lang) because its logic-level next-token model is single-byte —
exactly paper 2's calculator-grammar setting, where conditions 3 and 4 of
Definition 3.3 coincide. Multi-byte grammars (c_expr) would need a
longest-match logic spec in Pearlite (a much heavier effort) and so are
covered by the bounded Kani tier only. The generator skips the Creusot
crate automatically when any operator is multi-byte.

Trust chain for the unbounded tier: Rust → creusot-rustc → Coma → why3find
→ SMT solvers, plus one `#[trusted]` axiom that the operational
byte-scanning `next_token_lbp` matches its logic model. Install per
`verification/README.md` (Tier A).

## Statement/declaration host grammar (`src/host.rs`)

A miniature demonstration that the single-tree story scales past the
expression fragment — the piece both papers describe as future work. A
hand-written recursive-descent statement layer embeds the generated Pratt
expression core:

```
program := stmt*
stmt    := "let" IDENT "=" expr ";" | expr ";" | "{" stmt* "}"
expr    := the generated grammar's expression
```

`reparse_program` reparses the whole program tree (statements + expressions)
with **two granularities of reuse, into one tree**:

- **statement-level** — statements the edit doesn't touch are reused
  wholesale (`Arc::clone`), the "reparseable element" mechanism IntelliJ /
  Roslyn / SwiftSyntax use; and
- **expression-level** — inside the one edited statement, expression subtrees
  are reused via the precedence-bounded predicate (`incremental_parse`).

The fast path applies when the edit sits strictly inside one statement's
expression and adds no statement syntax; otherwise it falls back to a fresh
program parse (sound for any edit). `host_incremental_matches_fresh` checks
the result always equals a fresh program parse (50k edits × 4 grammars);
`host_reuse_demo` shows a one-digit edit in the middle of a 5-statement
program reusing the **4 untouched statements wholesale** and **12 expression
subtrees** inside the edited statement.

This is a demonstration, not a statement-grammar generator: the statement
syntax is fixed, blocks reparse wholesale rather than with intra-block reuse,
and strong-conflict constructs (dangling-else) and error recovery remain out
of scope, as in the papers.

## Scope

Supported: operator-expression fragment — int/ident atoms, one bracket
pair, prefix/postfix unary operators, infix binary operators with declared
precedence, associativity, and conflict class. Plus the fixed
statement/declaration host above (demonstration).

Out of scope (deliberately, matching the papers' boundaries): statements
and declarations / a recursive-descent host grammar; ternary, call, index,
and member syntax; strong-conflict escalation; error recovery; incremental
relexing; end-to-end (parser + recovery) verification.
