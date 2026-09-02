#!/usr/bin/env bash
# Enforce that the Apache-2.0 runtime has no proprietary Market awareness.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

boundary_pattern='coopd[_-]market|COOP_MARKET|/api/v1/config/market|tab-market|farm\.startcaas\.com'
config_pattern='COOP_MARKET|/api/v1/config/market|tab-market|farm\.startcaas\.com'
runtime_pattern='market'

if grep -RInE "$boundary_pattern" crates scripts .github \
  --exclude='check-open-core-boundary.sh'; then
  echo "error: proprietary Market surface leaked into the OSS runtime" >&2
  exit 1
fi

if grep -RInE "$config_pattern" docs; then
  echo "error: proprietary Market config/API/UI leaked into public docs" >&2
  exit 1
fi

if cargo metadata --no-deps --format-version 1 \
  | grep -Eq '"name":"coopd[-_]market"'; then
  echo "error: proprietary Market dependency leaked into Cargo metadata" >&2
  exit 1
fi

if grep -RIniE "$runtime_pattern" crates/coopd crates/coop-cli; then
  echo "error: OSS daemon/CLI must have zero Market awareness" >&2
  exit 1
fi

echo "open-core boundary: OK"
