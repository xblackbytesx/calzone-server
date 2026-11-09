# syntax=docker/dockerfile:1.7
# CalZone server image. Build context is the repo root:
#   docker compose -f docker/docker-compose-build.yml build
#
# Cross-compiles, never emulates: the builder runs on $BUILDPLATFORM and only
# targets $TARGETARCH (same approach as wordwarz-server and quantifyme-v2).

FROM --platform=$BUILDPLATFORM rust:1.92-bookworm AS builder
ARG TARGETARCH
WORKDIR /app

RUN set -eux; \
    case "$TARGETARCH" in \
      amd64) triple=x86_64-unknown-linux-gnu; cross="" ;; \
      arm64) triple=aarch64-unknown-linux-gnu; cross="gcc-aarch64-linux-gnu libc6-dev-arm64-cross" ;; \
      *) echo "unsupported TARGETARCH=$TARGETARCH" >&2; exit 1 ;; \
    esac; \
    echo "$triple" > /target-triple; \
    if [ -n "$cross" ]; then apt-get update; apt-get install -y --no-install-recommends $cross; rm -rf /var/lib/apt/lists/*; fi; \
    rustup target add "$triple"

ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar

# Dependency-cache layer: real manifests, dummy sources. Versions are
# neutralized so a release bump does not invalidate the whole crate graph.
# EVERY workspace member's manifest must be copied here: cargo loads the
# whole workspace first. CI checks this.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/client/Cargo.toml crates/client/
COPY crates/core/Cargo.toml crates/core/
COPY crates/dav/Cargo.toml crates/dav/
COPY crates/ffi/Cargo.toml crates/ffi/
COPY crates/server/Cargo.toml crates/server/
RUN set -eux; \
    sed -i '/^\[workspace.package\]/,/^\[/ s/^version = .*/version = "0.0.0"/' Cargo.toml; \
    mkdir -p crates/client/src crates/core/src crates/dav/src crates/ffi/src/bin crates/server/src; \
    echo '' > crates/client/src/lib.rs; \
    echo '' > crates/ffi/src/lib.rs; \
    echo 'fn main(){}' > crates/ffi/src/bin/uniffi-bindgen.rs; \
    echo '' > crates/core/src/lib.rs; \
    echo '' > crates/dav/src/lib.rs; \
    echo '' > crates/server/src/lib.rs; \
    echo 'fn main(){}' > crates/server/src/main.rs; \
    cargo build --release -p calzone-server --target "$(cat /target-triple)"; \
    rm -rf crates/*/src

# Real build. Migrations (and later templates and static assets) are
# embedded at compile time.
COPY . .
RUN set -eux; \
    triple="$(cat /target-triple)"; \
    touch crates/core/src/lib.rs crates/dav/src/lib.rs crates/server/src/lib.rs crates/server/src/main.rs; \
    cargo build --release -p calzone-server --target "$triple"; \
    cp "target/$triple/release/calzone-server" /calzone-server

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -g 1000 calzone \
    && useradd -u 1000 -g calzone -M -s /usr/sbin/nologin calzone
COPY --from=builder /calzone-server /usr/local/bin/calzone-server
# No volume: everything the server keeps lives in Postgres.
USER calzone
EXPOSE 8080
CMD ["calzone-server", "serve"]
