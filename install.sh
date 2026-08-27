#!/bin/sh
# Installs imogen, the command-line client for an imogen photo library.
#
#   curl -fsSL https://raw.githubusercontent.com/ergofobe/imogen-cli/main/install.sh | sh
#
# Downloads the build for this machine, checks it against the release's own SHA256SUMS,
# and puts the binary, its shell completions and its man page under ~/.local — no root, no
# package manager. Pass --deb or --rpm to let the system package manager own it instead.
#
# Everything is defined before anything runs, and the last line is the only thing that
# does anything, so a download cut short cannot leave a half-executed script behind.
set -eu

REPO="ergofobe/imogen-cli"
# Overridable so the installer can be pointed at a mirror, or at a local server for
# testing, without editing it.
BASE_URL="${IMOGEN_DOWNLOAD_BASE:-https://github.com/${REPO}/releases/download}"
API_URL="${IMOGEN_API_BASE:-https://api.github.com/repos/${REPO}}"

VERSION="${IMOGEN_VERSION:-}"
PREFIX="${IMOGEN_PREFIX:-$HOME/.local}"
METHOD="tarball"
VERIFY="yes"
TMPDIR_=""

say() { printf '%s\n' "$*" >&2; }
err() { printf 'error: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || err "this needs $1, which is not installed"; }

usage() {
  cat >&2 <<'USAGE'
imogen installer

  curl -fsSL https://raw.githubusercontent.com/ergofobe/imogen-cli/main/install.sh | sh

Options
  --version <tag>   Install this release rather than the newest (e.g. v0.1.0)
  --prefix <dir>    Where to install. Default ~/.local
  --deb             Install the .deb through dpkg, rather than a tarball
  --rpm             Install the .rpm through rpm, rather than a tarball
  --no-verify       Skip the checksum check. Do not.
  --help            This

Environment
  IMOGEN_VERSION, IMOGEN_PREFIX, IMOGEN_DOWNLOAD_BASE, IMOGEN_API_BASE
USAGE
}

cleanup() { [ -n "$TMPDIR_" ] && rm -rf "$TMPDIR_"; }

# --- what machine is this -------------------------------------------------------------

detect_os() {
  case "$(uname -s)" in
    Linux) echo linux ;;
    Darwin) echo macos ;;
    *) err "imogen has no build for $(uname -s). Build from source: https://github.com/${REPO}" ;;
  esac
}

detect_arch() {
  arch="$(uname -m)"
  # Under Rosetta a native arm64 Mac reports x86_64. Installing the Intel build would work
  # and would be needlessly slow, so ask whether this process is being translated.
  if [ "$arch" = "x86_64" ] && [ "$(uname -s)" = "Darwin" ] &&
     [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = "1" ]; then
    arch="arm64"
  fi
  case "$arch" in
    x86_64 | amd64) echo x86_64 ;;
    aarch64 | arm64) echo aarch64 ;;
    *) err "imogen has no build for $arch. Build from source: https://github.com/${REPO}" ;;
  esac
}

target_triple() {
  case "$1-$2" in
    linux-x86_64) echo x86_64-unknown-linux-musl ;;
    linux-aarch64) echo aarch64-unknown-linux-musl ;;
    macos-x86_64) echo x86_64-apple-darwin ;;
    macos-aarch64) echo aarch64-apple-darwin ;;
  esac
}

# --- fetching -------------------------------------------------------------------------

# The ordinary case is an https URL, and there curl is pinned to https so that a redirect
# cannot quietly downgrade the transport. An operator who has pointed IMOGEN_DOWNLOAD_BASE
# somewhere of their own is taken at their word.
curl_proto() {
  case "$1" in
    https://*) echo "--proto =https --tlsv1.2" ;;
    *) echo "" ;;
  esac
}

# One downloader, whichever the machine has. `-f` matters: without it curl writes the 404
# page to the file and reports success.
fetch() {
  if command -v curl >/dev/null 2>&1; then
    # Deliberately unquoted: these are two arguments, not one.
    # shellcheck disable=SC2046
    curl -fsSL --retry 3 $(curl_proto "$1") "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$1" -O "$2"
  else
    err "this needs curl or wget, and has neither"
  fi
}

fetch_stdout() {
  if command -v curl >/dev/null 2>&1; then
    # shellcheck disable=SC2046
    curl -fsSL --retry 3 $(curl_proto "$1") "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO- "$1"
  else
    err "this needs curl or wget, and has neither"
  fi
}

latest_version() {
  # Parsed with sed rather than jq, which is not installed on a fresh machine.
  fetch_stdout "${API_URL}/releases/latest" \
    | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
    | head -n 1
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | sed 's/.*= *//'
  else
    echo ""
  fi
}

verify() {
  file="$1"; name="$2"; sums="$3"
  expected="$(sed -n "s/^\([0-9a-f]\{64\}\)[[:space:]][[:space:]]*\**${name}$/\1/p" "$sums" | head -n 1)"
  [ -n "$expected" ] || err "$name is not listed in SHA256SUMS"

  actual="$(sha256_of "$file")"
  if [ -z "$actual" ]; then
    say "warning: no sha256 tool here, so the download could not be checked"
    return 0
  fi
  [ "$actual" = "$expected" ] || err "$name does not match its checksum — do not use it
  expected $expected
  got      $actual"
  say "checksum ok"
}

# --- installing -----------------------------------------------------------------------

install_tarball() {
  triple="$1"; version="$2"
  name="imogen-${triple}.tar.gz"

  say "downloading ${name} (${version})"
  fetch "${BASE_URL}/${version}/${name}" "${TMPDIR_}/${name}"
  if [ "$VERIFY" = "yes" ]; then
    fetch "${BASE_URL}/${version}/SHA256SUMS" "${TMPDIR_}/SHA256SUMS"
    verify "${TMPDIR_}/${name}" "$name" "${TMPDIR_}/SHA256SUMS"
  fi

  tar -xzf "${TMPDIR_}/${name}" -C "$TMPDIR_"
  unpacked="${TMPDIR_}/imogen-${triple}"
  [ -x "${unpacked}/imogen" ] || err "the archive did not contain what was expected"

  mkdir -p "${PREFIX}/bin"
  # Written to a neighbouring name and moved into place, so a running copy is replaced
  # atomically rather than being truncated underneath itself.
  cp "${unpacked}/imogen" "${PREFIX}/bin/.imogen.new"
  chmod 755 "${PREFIX}/bin/.imogen.new"
  mv -f "${PREFIX}/bin/.imogen.new" "${PREFIX}/bin/imogen"

  if [ -f "${unpacked}/man/imogen.1" ]; then
    mkdir -p "${PREFIX}/share/man/man1"
    cp "${unpacked}/man/imogen.1" "${PREFIX}/share/man/man1/imogen.1"
  fi
  copy_completion "${unpacked}/completions/imogen.bash" "${PREFIX}/share/bash-completion/completions/imogen"
  copy_completion "${unpacked}/completions/_imogen" "${PREFIX}/share/zsh/site-functions/_imogen"
  copy_completion "${unpacked}/completions/imogen.fish" "${PREFIX}/share/fish/vendor_completions.d/imogen.fish"

  INSTALLED="${PREFIX}/bin/imogen"
}

copy_completion() {
  [ -f "$1" ] || return 0
  mkdir -p "$(dirname "$2")"
  cp "$1" "$2"
}

# dpkg and rpm both want root. Asking sudo for it is only reasonable when somebody chose
# this path deliberately, which is why it is behind a flag rather than being the default.
as_root() {
  if [ "$(id -u)" = "0" ]; then
    "$@"
  elif command -v sudo >/dev/null 2>&1; then
    say "this needs root; running: sudo $*"
    sudo "$@"
  else
    err "this needs root, and sudo is not installed. Run it as root, or drop --deb/--rpm."
  fi
}

install_package() {
  kind="$1"; arch="$2"; version="$3"
  # cargo-deb and cargo-generate-rpm put the version in the filename, and cargo-deb adds
  # a Debian revision of 1. Derived from the tag rather than looked up, so a release that
  # changes that revision will need this changed too.
  number="${version#v}"
  if [ "$kind" = "deb" ]; then
    case "$arch" in x86_64) debarch=amd64 ;; aarch64) debarch=arm64 ;; esac
    name="imogen-cli_${number}-1_${debarch}.deb"
  else
    name="imogen-cli-${number}-1.${arch}.rpm"
  fi

  say "downloading ${name}"
  fetch "${BASE_URL}/${version}/${name}" "${TMPDIR_}/${name}"
  if [ "$VERIFY" = "yes" ]; then
    fetch "${BASE_URL}/${version}/SHA256SUMS" "${TMPDIR_}/SHA256SUMS"
    verify "${TMPDIR_}/${name}" "$name" "${TMPDIR_}/SHA256SUMS"
  fi

  if [ "$kind" = "deb" ]; then
    need dpkg
    as_root dpkg -i "${TMPDIR_}/${name}"
  else
    need rpm
    as_root rpm -Uvh --replacepkgs "${TMPDIR_}/${name}"
  fi
  INSTALLED="/usr/bin/imogen"
}

on_path() {
  case ":${PATH}:" in
    *":$1:"*) return 0 ;;
    *) return 1 ;;
  esac
}

# --- the whole thing ------------------------------------------------------------------

main() {
  while [ $# -gt 0 ]; do
    case "$1" in
      --version) VERSION="${2:?--version needs a tag}"; shift 2 ;;
      --version=*) VERSION="${1#*=}"; shift ;;
      --prefix) PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
      --prefix=*) PREFIX="${1#*=}"; shift ;;
      --deb) METHOD="deb"; shift ;;
      --rpm) METHOD="rpm"; shift ;;
      --no-verify) VERIFY="no"; shift ;;
      -h | --help) usage; exit 0 ;;
      *) err "unknown option: $1 (try --help)" ;;
    esac
  done

  [ "$VERIFY" = "yes" ] || say "warning: --no-verify given; the download will not be checked"

  need tar
  os="$(detect_os)"
  arch="$(detect_arch)"
  triple="$(target_triple "$os" "$arch")"

  if [ "$METHOD" != "tarball" ] && [ "$os" != "linux" ]; then
    err "--${METHOD} is for Linux; on macOS the tarball is the only package"
  fi

  if [ -z "$VERSION" ]; then
    say "looking up the newest release"
    VERSION="$(latest_version)"
    [ -n "$VERSION" ] || err "could not work out the newest release; pass --version"
  fi

  say "imogen ${VERSION} · ${os} ${arch}"
  TMPDIR_="$(mktemp -d)"
  trap cleanup EXIT INT TERM

  case "$METHOD" in
    tarball) install_tarball "$triple" "$VERSION" ;;
    deb | rpm) install_package "$METHOD" "$arch" "$VERSION" ;;
  esac

  say ""
  say "installed $("$INSTALLED" --version 2>/dev/null || echo imogen) at ${INSTALLED}"

  bindir="$(dirname "$INSTALLED")"
  if ! on_path "$bindir"; then
    say ""
    say "${bindir} is not on your PATH. Add it:"
    say "  echo 'export PATH=\"${bindir}:\$PATH\"' >> ~/.profile"
  fi
  say ""
  say "Start with:  imogen login --server https://photos.example.com"
}

main "$@"
