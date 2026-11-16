#!/usr/bin/env bash
# eslabon: build a release tarball of the public `eslabon` binary (P7.1).
#
#   bash scripts/eslabon/release/build.sh [--target <target>] [--static]
#
# `--target` forces one of: macos-arm64, macos-x86_64, linux-x86_64, linux-arm64.
# Without it the target comes from `uname -s`/`uname -m`.
#
# `--static` (Linux only) builds against the musl triple. The workspace keeps
# rustls for its own HTTP transport, but the shared HTTP client still compiles
# `native-tls` (openssl-sys on Linux), so a musl build needs static OpenSSL
# headers and libraries on the machine. When the musl build fails this script
# stops with cargo's error instead of forcing a binary.
#
# The tarball is `dist/eslabon-<version>-<target>.tar.gz` (binary named
# `eslabon` + LICENSE + NOTICE + README.md) with a sibling `.sha256` file, and
# the smoke harness runs against the packaged binary before anything is kept.
#
# Signing is opt-in: `ESLABON_APPLE_IDENTITY` codesigns the macOS binary and
# `ESLABON_NOTARY_PROFILE` submits it to the notary service. Neither variable is
# printed; with them unset nothing is signed.
set -euo pipefail

RAIZ_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
DIR_DIST="$RAIZ_REPO/dist"

uso() {
  echo "usage: bash scripts/eslabon/release/build.sh [--target <target>] [--static]" >&2
  echo "targets: macos-arm64 macos-x86_64 linux-x86_64 linux-arm64" >&2
}

OBJETIVO=""
ESTATICO=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --target)
      [ "$#" -ge 2 ] || { uso; exit 2; }
      OBJETIVO="$2"
      shift 2
      ;;
    --static)
      ESTATICO=1
      shift
      ;;
    -h|--help)
      uso
      exit 0
      ;;
    *)
      uso
      exit 2
      ;;
  esac
done

detectar_objetivo() {
  local sistema arquitectura
  sistema="$(uname -s)"
  arquitectura="$(uname -m)"
  case "$sistema" in
    Darwin)
      case "$arquitectura" in
        arm64|aarch64) echo "macos-arm64" ;;
        x86_64) echo "macos-x86_64" ;;
      esac
      ;;
    Linux)
      case "$arquitectura" in
        x86_64|amd64) echo "linux-x86_64" ;;
        aarch64|arm64) echo "linux-arm64" ;;
      esac
      ;;
  esac
}

if [ -z "$OBJETIVO" ]; then
  OBJETIVO="$(detectar_objetivo)"
fi

case "$OBJETIVO" in
  macos-arm64|macos-x86_64|linux-x86_64|linux-arm64) ;;
  *)
    echo "error: unknown target «$OBJETIVO»" >&2
    uso
    exit 2
    ;;
esac

# eslabon: a full release build needs room for the workspace; refusing below
# 15 GB keeps a failed build from filling the disk.
if command -v df >/dev/null 2>&1; then
  LIBRES_MB="$(df -k "$RAIZ_REPO" | awk 'NR==2 {print int($4/1024)}')"
  if [ -n "$LIBRES_MB" ] && [ "$LIBRES_MB" -lt 15360 ]; then
    echo "error: only ${LIBRES_MB} MB free; a release build needs at least 15 GB" >&2
    exit 1
  fi
fi

TRIPLE=""
DIR_DESTINO="$RAIZ_REPO/codex-rs/target/release"
if [ "$ESTATICO" -eq 1 ]; then
  case "$OBJETIVO" in
    linux-*) ;;
    *)
      echo "error: --static is only for Linux targets (this is «$OBJETIVO»)" >&2
      exit 2
      ;;
  esac
  case "$OBJETIVO" in
    linux-x86_64) TRIPLE="x86_64-unknown-linux-musl" ;;
    linux-arm64) TRIPLE="aarch64-unknown-linux-musl" ;;
  esac
  if ! command -v rustup >/dev/null 2>&1; then
    echo "error: rustup is required for --static; install it from https://rustup.rs" >&2
    exit 1
  fi
  if ! rustup target list --installed | grep -qx "$TRIPLE"; then
    echo "error: the $TRIPLE target is not installed; add it with:" >&2
    echo "  rustup target add $TRIPLE" >&2
    exit 1
  fi
  DIR_DESTINO="$RAIZ_REPO/codex-rs/target/$TRIPLE/release"
fi

export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0
# eslabon: releases keep no debug info (see install.sh); lean on the machine.
export CARGO_PROFILE_RELEASE_DEBUG=none
export RUST_MIN_STACK=8388608

echo "== building eslabon for $OBJETIVO${TRIPLE:+ ($TRIPLE)}"
cd "$RAIZ_REPO/codex-rs"
if [ -n "$TRIPLE" ]; then
  cargo build --release -p codex-cli --bin eslabon --target "$TRIPLE"
else
  cargo build --release -p codex-cli --bin eslabon
fi

BINARIO="$DIR_DESTINO/eslabon"
[ -x "$BINARIO" ] || { echo "error: cargo did not produce $BINARIO" >&2; exit 1; }

TMP="$(mktemp -d "${TMPDIR:-/tmp}/eslabon-release.XXXXXX")"
limpiar() {
  rm -rf "$TMP"
}
trap limpiar EXIT INT TERM

if command -v strip >/dev/null 2>&1; then
  strip "$BINARIO"
fi

if [ -n "${ESLABON_APPLE_IDENTITY:-}" ]; then
  case "$OBJETIVO" in
    macos-*)
      # eslabon: hardened runtime is what the notary service expects; sign
      # before packaging so the tarball ships a signed binary.
      codesign --force --options runtime --timestamp \
        --sign "$ESLABON_APPLE_IDENTITY" "$BINARIO"
      ;;
    *)
      echo "warning: ESLABON_APPLE_IDENTITY is set but the target is not macOS" >&2
      ;;
  esac
fi

if [ -n "${ESLABON_NOTARY_PROFILE:-}" ]; then
  case "$OBJETIVO" in
    macos-*)
      ZIP="$TMP/eslabon-notarizar.zip"
      (cd "$DIR_DESTINO" && zip -q "$ZIP" eslabon)
      if ! xcrun notarytool submit "$ZIP" \
        --keychain-profile "$ESLABON_NOTARY_PROFILE" --wait; then
        echo "error: the notary service rejected $ZIP" >&2
        exit 1
      fi
      ;;
    *)
      echo "warning: ESLABON_NOTARY_PROFILE is set but the target is not macOS" >&2
      ;;
  esac
fi

if [ -z "$(command -v shasum || true)" ] && [ -z "$(command -v sha256sum || true)" ]; then
  echo "error: need shasum or sha256sum to write the checksum" >&2
  exit 1
fi

hash_de_archivo() {
  local archivo="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$archivo"
  else
    shasum -a 256 "$archivo"
  fi
}

VERSION="$(ESLABON_HOME="$TMP/home" CODEX_HOME="$TMP/home" \
  "$BINARIO" --version </dev/null | head -n 1 | awk '{print $2}')"
[ -n "$VERSION" ] || { echo "error: could not read the version from $BINARIO" >&2; exit 1; }

DIR_PAQUETE="$TMP/eslabon-$VERSION-$OBJETIVO"
mkdir -p "$DIR_PAQUETE" "$DIR_DIST"
cp "$BINARIO" "$DIR_PAQUETE/eslabon"
chmod +x "$DIR_PAQUETE/eslabon"
cp "$RAIZ_REPO/LICENSE" "$RAIZ_REPO/NOTICE" "$RAIZ_REPO/README.md" "$DIR_PAQUETE/"

TAR="$DIR_DIST/eslabon-$VERSION-$OBJETIVO.tar.gz"
tar -czf "$TAR" -C "$DIR_PAQUETE" eslabon LICENSE NOTICE README.md
(cd "$DIR_DIST" && hash_de_archivo "$(basename "$TAR")" > "$(basename "$TAR").sha256")

echo "== packaged $TAR"
cat "$TAR.sha256"

# eslabon: the acceptance proof is the packaged binary itself; extract it
# somewhere disposable and run the smoke harness against it.
DIR_HUMO="$TMP/humo"
mkdir -p "$DIR_HUMO"
tar -xzf "$TAR" -C "$DIR_HUMO"
if ! bash "$RAIZ_REPO/scripts/eslabon/humo/humo.sh" "$DIR_HUMO/eslabon"; then
  echo "error: the packaged binary failed the smoke harness" >&2
  exit 1
fi

echo "release: $TAR"
