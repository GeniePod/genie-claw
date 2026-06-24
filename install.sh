#!/usr/bin/env sh
# GenieClaw installer. Downloads the prebuilt runtime binaries for this machine
# from the latest GitHub Release, verifies their checksum, and installs them.
#
#   curl -fsSL https://github.com/GeniePod/genie-claw/releases/latest/download/install.sh | sh
#
# Environment overrides:
#   GENIECLAW_VERSION  pin a version, e.g. v1.0.0-rc.1   (default: latest release)
#   GENIECLAW_PREFIX   install prefix                    (default: /usr/local, or
#                                                          ~/.local if not writable)
#
# Installs the five runtime binaries (genie-core, genie-ctl, genie-api,
# genie-governor, genie-health) to $PREFIX/bin and a starter config to
# ~/.config/geniepod/geniepod.toml. The full Jetson/voice/Home-Assistant setup
# stays in deploy/setup-jetson.sh — this is the agent runtime only.

set -eu

REPO="GeniePod/genie-claw"
RELEASES="https://github.com/${REPO}/releases"

info() { printf '\033[1;34m==>\033[0m %s\n' "$1"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$1" >&2; }
err()  { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || err "required tool not found: $1"; }
need uname
need tar
need install
# one of curl/wget for downloads
if command -v curl >/dev/null 2>&1; then DL="curl -fsSL -o"
elif command -v wget >/dev/null 2>&1; then DL="wget -qO"
else err "need curl or wget"; fi
# one of sha256sum/shasum for verification
if command -v sha256sum >/dev/null 2>&1; then SHA="sha256sum"
elif command -v shasum >/dev/null 2>&1; then SHA="shasum -a 256"
else err "need sha256sum or shasum"; fi

# --- detect platform ---------------------------------------------------------
os="$(uname -s)"
[ "$os" = "Linux" ] || err "unsupported OS: $os (this release ships Linux binaries only)"
arch="$(uname -m)"
case "$arch" in
  aarch64|arm64)  target="aarch64-unknown-linux-gnu" ;;
  x86_64|amd64)   target="x86_64-unknown-linux-gnu" ;;
  *) err "unsupported architecture: $arch (supported: aarch64, x86_64)" ;;
esac
info "Platform: ${os} ${arch} -> ${target}"

# --- resolve version ---------------------------------------------------------
# Default to the latest STABLE release via the /releases/latest redirect (this
# excludes pre-releases). During the rc/beta phase, pin GENIECLAW_VERSION.
version="${GENIECLAW_VERSION:-}"
if [ -z "$version" ]; then
  info "Resolving latest release..."
  if command -v curl >/dev/null 2>&1; then
    version="$(curl -fsSLo /dev/null -w '%{url_effective}' "${RELEASES}/latest" 2>/dev/null | sed -n 's#.*/tag/##p')"
  else
    version="$(wget -qO- "${RELEASES}/latest" 2>/dev/null | sed -n 's#.*/releases/tag/\(v[0-9][^"<> ]*\).*#\1#p' | head -n1)"
  fi
fi
case "${version:-}" in
  v*) : ;;
  *) err "no stable release found. During the pre-release phase, pin a version, e.g.:
     GENIECLAW_VERSION=v1.0.0-rc.1 sh install.sh" ;;
esac
info "Version: ${version}"

tarball="genieclaw-${version#v}-${target}.tar.gz"
base="${RELEASES}/download/${version}"

# --- download + verify -------------------------------------------------------
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
info "Downloading ${tarball}..."
$DL "${tmp}/${tarball}" "${base}/${tarball}" || err "download failed: ${base}/${tarball}"
if $DL "${tmp}/SHA256SUMS" "${base}/SHA256SUMS" 2>/dev/null; then
  expected="$(grep " .*${tarball}\$" "${tmp}/SHA256SUMS" | awk '{print $1}' | head -n1)"
  [ -n "$expected" ] || err "no checksum for ${tarball} in SHA256SUMS"
  actual="$(cd "$tmp" && $SHA "$tarball" | awk '{print $1}')"
  [ "$expected" = "$actual" ] || err "checksum mismatch for ${tarball} (expected ${expected}, got ${actual})"
  info "Checksum verified."
else
  warn "SHA256SUMS not found for ${version}; skipping checksum verification."
fi

# --- install -----------------------------------------------------------------
tar -C "$tmp" -xzf "${tmp}/${tarball}"
src="${tmp}/genieclaw-${version#v}-${target}"

prefix="${GENIECLAW_PREFIX:-/usr/local}"
bindir="${prefix}/bin"
SUDO=""
if ! mkdir -p "$bindir" 2>/dev/null || ! [ -w "$bindir" ]; then
  if [ "$prefix" = "/usr/local" ] && command -v sudo >/dev/null 2>&1 && [ "$(id -u)" -ne 0 ]; then
    SUDO="sudo"; info "Using sudo to install into ${bindir}"
  else
    prefix="${HOME}/.local"; bindir="${prefix}/bin"; mkdir -p "$bindir"
    warn "no write access to /usr/local; installing into ${bindir}"
  fi
fi

for bin in genie-core genie-ctl genie-api genie-governor genie-health; do
  $SUDO install -m 0755 "${src}/bin/${bin}" "${bindir}/${bin}"
done
info "Installed 5 binaries to ${bindir}"

cfgdir="${HOME}/.config/geniepod"
cfg="${cfgdir}/geniepod.toml"
if [ ! -f "$cfg" ]; then
  mkdir -p "$cfgdir"
  install -m 0644 "${src}/config/geniepod.toml" "$cfg"
  info "Wrote starter config to ${cfg}"
else
  info "Keeping existing config at ${cfg}"
fi

# --- next steps --------------------------------------------------------------
echo
info "GenieClaw ${version} installed."
case ":${PATH}:" in
  *":${bindir}:"*) : ;;
  *) warn "add ${bindir} to your PATH:  export PATH=\"${bindir}:\$PATH\"" ;;
esac
cat <<EOF

Next:
  genie-ctl --help
  GENIEPOD_CONFIG=${cfg} genie-core      # start the agent runtime + HTTP API

Jetson voice / Home Assistant setup: see GETTING_STARTED.md in the repo.
EOF
