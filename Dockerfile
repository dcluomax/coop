# Multi-stage build → a small Debian-slim runtime image holding the static-ish
# `coopd` daemon + `coop` CLI. The runtime carries `bubblewrap` (per-hen bash
# sandbox) and `tmux` (persistent PTY sessions) so the Farm UI shell works.

# ---- builder ---------------------------------------------------------------
FROM rust:1.91-bookworm AS builder
WORKDIR /src
# Cache deps: copy manifests first.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo build --release --bin coopd --bin coop

# ---- runtime ---------------------------------------------------------------
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends bubblewrap tmux ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --create-home --home-dir /home/coop --shell /usr/sbin/nologin coop \
 && mkdir -p /data && chown coop:coop /data

COPY --from=builder /src/target/release/coopd /usr/local/bin/coopd
COPY --from=builder /src/target/release/coop  /usr/local/bin/coop

USER coop
ENV COOP_DATA_DIR=/data
VOLUME ["/data"]
EXPOSE 9700

# Healthcheck uses the CLI against the loopback interface (the /healthz route
# is always auth-exempt and loopback Host is always allowed).
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD ["coop", "health"]

ENTRYPOINT ["coopd"]
CMD ["serve", "--addr", "0.0.0.0:9700"]
