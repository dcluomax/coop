#!/usr/bin/env sh
# Coop installer — downloads the right pre-built `coopd` + `coop` binaries for
# this machine from the latest GitHub release, verifies the SHA-256 checksum,
# and installs them.
#
#   curl -fsSL https://raw.githubusercontent.com/dcluomax/coop/main/scripts/install.sh | sh
#
# Environment overrides:
#   COOP_VERSION      Release tag to install (default: latest, e.g. v0.1.0-alpha.1)
#   COOP_INSTALL_DIR  Where to put the binaries (default: /usr/local/bin, falling
#                     back to ~/.local/bin when that isn't writable)
#   COOP_REPO         owner/repo to download from (default: dcluomax/coop)
#
# POSIX sh — no bashisms; works on macOS, Linux, and Raspberry Pi OS.
set -eu

REPO="${COOP_REPO:-dcluomax/coop}"
VERSION="${COOP_VERSION:-latest}"

say()  { printf '\033[1;36m==>\033[0m %s\n' "$1"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$1" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "required tool not found: $1"; }
need uname
need tar
# A downloader: prefer curl, fall back to wget.
if command -v curl >/dev/null 2>&1; then
  DL='curl -fsSL'
  DL_O='curl -fsSL -o'
elif command -v wget >/dev/null 2>&1; then
  DL='wget -qO-'
  DL_O='wget -qO'
else
  die "need either curl or wget"
fi

# --- Detect platform → Rust target triple ----------------------------------
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux)  os_part="unknown-linux-gnu" ;;
  Darwin) os_part="apple-darwin" ;;
  *) die "unsupported OS: $os (Windows users: download the .zip from the releases page)" ;;
esac
case "$arch" in
  x86_64|amd64)          arch_part="x86_64" ;;
  aarch64|arm64)         arch_part="aarch64" ;;
  armv7l|armv7|armhf)    arch_part="armv7"; [ "$os" = "Linux" ] && os_part="unknown-linux-gnueabihf" ;;
  *) die "unsupported architecture: $arch" ;;
esac
TARGET="${arch_part}-${os_part}"

# --- Resolve version --------------------------------------------------------
if [ "$VERSION" = "latest" ]; then
  say "Resolving latest release of $REPO..."
  api="https://api.github.com/repos/${REPO}/releases/latest"
  tag="$($DL "$api" | grep -m1 '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
  [ -n "$tag" ] || die "could not determine the latest release tag (set COOP_VERSION to override)"
  VERSION="$tag"
fi
say "Installing Coop $VERSION for $TARGET"

asset="coop-${VERSION}-${TARGET}.tar.gz"
base="https://github.com/${REPO}/releases/download/${VERSION}"

# --- Download + verify ------------------------------------------------------
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM
say "Downloading $asset"
$DL_O "$tmp/$asset"        "$base/$asset"        || die "download failed: $base/$asset"
$DL_O "$tmp/$asset.sha256" "$base/$asset.sha256" || warn "no checksum published; skipping verification"

if [ -s "$tmp/$asset.sha256" ]; then
  say "Verifying SHA-256 checksum"
  expected="$(awk '{print $1}' "$tmp/$asset.sha256")"
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
  else
    actual=""; warn "no sha256sum/shasum available; skipping verification"
  fi
  if [ -n "$actual" ] && [ "$expected" != "$actual" ]; then
    die "checksum mismatch! expected $expected, got $actual"
  fi
fi

# --- Extract ----------------------------------------------------------------
say "Extracting"
tar -xzf "$tmp/$asset" -C "$tmp"
# The archive contains a top-level dir: coop-<version>-<target>/{coopd,coop}
srcdir="$tmp/coop-${VERSION}-${TARGET}"
[ -d "$srcdir" ] || srcdir="$(find "$tmp" -maxdepth 1 -type d -name 'coop-*' | head -n1)"
[ -x "$srcdir/coopd" ] || die "coopd binary not found in archive"

# --- Choose install dir -----------------------------------------------------
if [ -n "${COOP_INSTALL_DIR:-}" ]; then
  dir="$COOP_INSTALL_DIR"
elif [ -w /usr/local/bin ] 2>/dev/null; then
  dir="/usr/local/bin"
elif [ "$(id -u)" = "0" ]; then
  dir="/usr/local/bin"
else
  dir="$HOME/.local/bin"
fi
mkdir -p "$dir" 2>/dev/null || die "cannot create install dir: $dir"

install_bin() {
  src="$1"; name="$(basename "$1")"
  if [ -w "$dir" ]; then
    cp "$src" "$dir/$name" && chmod +x "$dir/$name"
  elif command -v sudo >/dev/null 2>&1; then
    warn "$dir is not writable; using sudo"
    sudo cp "$src" "$dir/$name" && sudo chmod +x "$dir/$name"
  else
    die "cannot write to $dir (set COOP_INSTALL_DIR to a writable path)"
  fi
}
say "Installing coopd + coop to $dir"
install_bin "$srcdir/coopd"
install_bin "$srcdir/coop"

# --- Done -------------------------------------------------------------------
say "Installed:"
printf '    %s\n' "$dir/coopd" "$dir/coop"
case ":$PATH:" in
  *":$dir:"*) ;;
  *) warn "$dir is not on your PATH — add it, e.g.: export PATH=\"$dir:\$PATH\"" ;;
esac
cat <<EOF

Next:
  coopd serve &            # start the daemon on http://127.0.0.1:9700
  coop hen list            # talk to it
  open http://127.0.0.1:9700/   # Farm UI

Full quickstart: https://github.com/${REPO}/blob/main/docs/quickstart.md
EOF
