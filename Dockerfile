# Pipeline · multi-stage build · single static-ish binary out the other side.
# Image size cap = 200 MB (gates.image_size_mb in pipeline.yaml).

# ---------- builder ----------
# ! Must match rust-toolchain.toml (1.97.0). It did NOT — the base was 1.94 while the
# pin said 1.97 — so every image build made rustup download a SECOND toolchain at build
# time. Slow, and a hard dependency on static.rust-lang.org being reachable from inside
# buildkit: when that fetch failed the whole build died, having compiled nothing. It also
# quietly reintroduced the drift the pin exists to prevent, since the image was the one
# place the pinned compiler wasn't guaranteed. Bump both together, or not at all.
FROM rust:1.97-slim-bookworm AS builder

ENV CARGO_TERM_COLOR=never \
    CARGO_NET_RETRY=4 \
    RUSTFLAGS="-C strip=symbols"

WORKDIR /usr/src/pipeline

# Build dependencies first for cache reuse.
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates \
 && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock rust-toolchain.toml rustfmt.toml clippy.toml ./
COPY crates ./crates

# ! ✗ append `|| true` here. It was `cargo build && strip || true`, which is
# not if-then-else (SC2015): a FAILED cargo build fell through to `|| true` and
# the layer reported success. The binary is already stripped by
# RUSTFLAGS="-C strip=symbols" above, so the explicit strip was redundant too.
RUN cargo build --release --bin pipeline

# ---------- runtime ----------
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git curl \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd -r pipeline && useradd -r -g pipeline -u 10001 pipeline

WORKDIR /app
COPY --from=builder /usr/src/pipeline/target/release/pipeline /usr/local/bin/pipeline

# ! The runtime writes everything durable under /work/.pipeline — memory.db,
# sessions, digests, OAuth access+refresh tokens. compose mounts a named volume
# there and a bind mount at /work, and BOTH land root-owned by default while the
# process runs as uid 10001. Every write then fails; the OAuth store and the
# memory layer are best-effort, so they fail SILENTLY — the deployment looks
# healthy while persisting nothing (it did, for 10 days).
#
# Creating these in the image with the right owner makes a FRESH named volume
# inherit that ownership when Docker initialises it. A pre-existing volume, or a
# host bind mount, must be chowned to 10001:999 once — Docker never re-inits an
# already-populated volume, and never touches bind-mount ownership at all.
RUN mkdir -p /work/.pipeline \
 && chown -R pipeline:pipeline /work

USER pipeline
WORKDIR /work

ENTRYPOINT ["/usr/local/bin/pipeline"]
CMD ["--help"]
