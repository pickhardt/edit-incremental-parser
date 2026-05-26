# Reproduction image for incremental-pratt-gen.
#
# Covers the generator and three of the four claim targets: gen, proptest
# (soundness oracle), verify (bounded Kani certificate), and bench. The
# unbounded Creusot tier (`make verify-creusot`) is intentionally NOT in
# this image: it needs the Creusot toolchain (opam + a Why3 git build +
# a pinned Rust nightly, ~15 min from source), which the companion artifact
# (verification/README.md, Tier A) installs on the host. The Kani tier in this
# image is the bit-precise, fully-containerized check.
#
# Build:  docker build -t ipg .
# Run:    docker run --rm ipg            # default: make smoke
#         docker run --rm ipg make all   # gen + proptest + verify + bench
#
# Note: pin the base image by digest for a byte-stable AEC artifact.
FROM rust:1.84-bookworm

# Kani's backend (CBMC) is fetched by `cargo kani setup`.
RUN cargo install --locked kani-verifier \
    && cargo kani setup

WORKDIR /work
COPY . /work

# Pre-build the generator so the first `make` is fast.
RUN cargo build -p incremental-pratt-gen

CMD ["make", "smoke"]
