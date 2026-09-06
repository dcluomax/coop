#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
TARGET="${1:?usage: build-linux-release.sh <x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu>}"
case "$TARGET" in
  x86_64-unknown-linux-gnu) PLATFORM="linux/amd64" ;;
  aarch64-unknown-linux-gnu) PLATFORM="linux/arm64" ;;
  *) echo "unsupported native Linux release target: $TARGET" >&2; exit 1 ;;
esac

CARGO_CACHE="${CARGO_HOME:-$HOME/.cargo}"
mkdir -p "$CARGO_CACHE"

# Keep the baseline build separate from host-linked artifacts restored by CI.
# Both executables must run inside Bookworm before they can be published.
docker run --rm --platform "$PLATFORM" \
  --user "$(id -u):$(id -g)" \
  -e CARGO_HOME=/cargo-cache \
  -e RUSTUP_TOOLCHAIN=1.91.1 \
  -e CARGO_TARGET_DIR=/src/target/bookworm \
  -e CARGO_INCREMENTAL=0 \
  -e CARGO_BUILD_JOBS -e CARGO_TERM_COLOR -e RUSTFLAGS \
  -v "$ROOT:/src" -v "$CARGO_CACHE:/cargo-cache" \
  -w /src rust:1.91.1-bookworm \
  bash -euc '
    cargo build --locked --release --target "$1" --bin coopd --bin coop
    "$CARGO_TARGET_DIR/$1/release/coopd" --version
    "$CARGO_TARGET_DIR/$1/release/coop" --version
    mkdir -p "target/$1/release"
    cp "$CARGO_TARGET_DIR/$1/release/coopd" "target/$1/release/coopd"
    cp "$CARGO_TARGET_DIR/$1/release/coop" "target/$1/release/coop"
  ' -- "$TARGET"
