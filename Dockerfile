# Pipeline · multi-stage build · single static-ish binary out the other side.
# Image size cap = 200 MB (gates.image_size_mb in pipeline.yaml).

# ---------- builder ----------
FROM rust:1.94-slim-bookworm AS builder

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

USER pipeline

ENTRYPOINT ["/usr/local/bin/pipeline"]
CMD ["--help"]
