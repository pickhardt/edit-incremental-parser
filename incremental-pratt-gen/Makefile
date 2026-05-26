# incremental-pratt-gen — AEC reproduction harness.
#
# Quick start:  make smoke      (kick-the-tires, < 60s)
# Full run:     make all        (gen + proptest + verify + bench)
#
# Targets map 1:1 to the claims table in README.md.

GRAMMARS := calc c_expr assoc_stress stmt_lang
GEN := ./target/debug/gen
ORACLE_CASES ?= 50000

.PHONY: all smoke gen proptest verify verify-creusot verify-all bench table clean

all: gen proptest verify bench
	@echo "== all targets complete =="

## Build the generator and emit one parser crate per grammar.
gen:
	cargo build -p incremental-pratt-gen
	@for g in $(GRAMMARS); do $(GEN) grammars/$$g.toml out/$$g; done

## Kick-the-tires: generate, build, and run a short oracle pass. < 60s.
smoke: gen
	@for g in $(GRAMMARS); do \
		echo "--- smoke: $$g ---"; \
		( cd out/$$g && cargo build --release --quiet && \
		  ORACLE_CASES=500 cargo test --release --quiet --test oracle 2>&1 | grep "test result" ); \
	done
	@echo "== smoke OK =="

## Property tests: the soundness oracle (incremental reparse == fresh parse,
## $(ORACLE_CASES) cases x2 x grammar) plus the token-store splice/lookup
## oracle and the predicate unit checks (cargo test --lib).
proptest: gen
	@for g in $(GRAMMARS); do \
		echo "--- proptest: $$g ($(ORACLE_CASES) oracle cases) ---"; \
		( cd out/$$g && ORACLE_CASES=$(ORACLE_CASES) cargo test --release --quiet 2>&1 | grep -E "test result|FAILED|DIVERGENCE" ); \
	done

## Machine-checked reuse-predicate certificate, re-discharged per grammar via Kani.
verify: gen
	@command -v cargo-kani >/dev/null 2>&1 || { echo "kani not installed: see README (cargo install --locked kani-verifier && cargo kani setup)"; exit 1; }
	@for g in $(GRAMMARS); do \
		echo "--- kani: $$g ---"; \
		( cd out/$$g && cargo kani 2>&1 | grep -E "VERIFICATION|harnesses, |Verification Time" ); \
	done

## Unbounded SMT certificate via Creusot (single-byte-operator grammars only;
## multi-byte grammars keep the bounded Kani tier). Needs the Creusot
## toolchain — see verification/README.md for install.
verify-creusot: gen
	@command -v cargo-creusot >/dev/null 2>&1 || { echo "creusot not installed: see verification/README.md (Tier A install)"; exit 1; }
	@for g in $(GRAMMARS); do \
		if [ -d out/$$g/creusot ]; then \
			echo "--- creusot: $$g ---"; \
			( cd out/$$g/creusot && cargo creusot 2>&1 | grep -E "Proved|unproved|error" ); \
		else \
			echo "--- creusot: $$g (skipped: multi-byte operators) ---"; \
		fi; \
	done

## Both certificate tiers: bounded (Kani, all grammars) then unbounded
## (Creusot, single-byte grammars).
verify-all: verify verify-creusot
	@echo "== both certificate tiers complete =="

## Headline benchmark: fresh vs. reparse latency, speedup, reuse %.
bench: gen
	@echo "grammar,bytes,fresh_us,reparse_us,speedup,reuse_pct"
	@for g in $(GRAMMARS); do \
		( cd out/$$g && cargo run --release --quiet --bin bench -- $$g 2>/dev/null | tail -n +2 ); \
	done

clean:
	rm -rf out target
	@for g in $(GRAMMARS); do rm -rf out/$$g; done
