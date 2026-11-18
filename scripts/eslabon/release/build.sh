#!/usr/bin/env bash
# eslabon: build a release tarball of the public `eslabon` binary (P7.1).
#
#   bash scripts/eslabon/release/build.sh [--target <target>] [--static]
#
# `--target` forces one of: macos-arm64, macos-x86_64, linux-x86_64, linux-arm64,
# windows-x86_64.
# Without it the target comes from `uname -s`/`uname -m`.
#
# `--static` (Linux only) builds against the musl triple. The workspace keeps
# rustls for its own HTTP transport, but the shared HTTP client still compiles
# `native-tls` (openssl-sys on Linux), so a musl build needs static OpenSSL
# headers and libraries on the machine. When the musl build fails this script
# stops with cargo's error instead of forcing a binary.
#
# The archive is `dist/eslabon-<version>-<target>.tar.gz` (or `.zip` for
# Windows) with the binary (`eslabon` or `eslabon.exe`) plus LICENSE, NOTICE
# and README.md, and a sibling `.sha256` file;
# the smoke harness runs against the packaged binary before anything is kept
# (macOS and Linux only).
#
# Signing is opt-in: `ESLABON_APPLE_IDENTITY` codesigns the macOS binary and
# `ESLABON_NOTARY_PROFILE` submits it to the notary service. Neither variable is
# printed; with them unset nothing is signed.
set -euo pipefail

RAIZ_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
DIR_DIST="$RAIZ_REPO/dist"

uso() {
  echo "usage: bash scripts/eslabon/release/build.sh [--target <target>] [--static]" >&2
  echo "targets: macos-arm64 macos-x86_64 linux-x86_64 linux-arm64 windows-x86_64" >&2
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
    MINGW*|MSYS*|CYGWIN*)
      echo "windows-x86_64"
      ;;
  esac
}

if [ -z "$OBJETIVO" ]; then
  OBJETIVO="$(detectar_objetivo)"
fi

case "$OBJETIVO" in
  macos-arm64|macos-x86_64|linux-x86_64|linux-arm64|windows-x86_64) ;;
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
ZIGBUILD=0
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
  if [ "$(uname -s)" != Linux ]; then
    if ! command -v cargo-zigbuild >/dev/null 2>&1 || ! command -v zig >/dev/null 2>&1; then
      echo "error: --static cross-builds need cargo-zigbuild and zig" >&2
      exit 1
    fi
    ZIGBUILD=1
  fi
fi

if [ -z "$TRIPLE" ]; then
  case "$OBJETIVO" in
    macos-x86_64)
      if [ "$(uname -s)/$(uname -m)" != Darwin/x86_64 ]; then
        TRIPLE="x86_64-apple-darwin"
      fi
      ;;
    macos-arm64)
      if [ "$(uname -s)/$(uname -m)" != Darwin/arm64 ]; then
        TRIPLE="aarch64-apple-darwin"
      fi
      ;;
    windows-x86_64)
      TRIPLE="x86_64-pc-windows-gnu"
      ;;
  esac
fi

if [ -n "$TRIPLE" ]; then
  DIR_DESTINO="$RAIZ_REPO/codex-rs/target/$TRIPLE/release"
  if command -v rustup >/dev/null 2>&1 \
     && ! rustup target list --installed | grep -qx "$TRIPLE"; then
    echo "error: the $TRIPLE target is not installed; add it with:" >&2
    echo "  rustup target add $TRIPLE" >&2
    exit 1
  fi
fi

if [ "$OBJETIVO" = windows-x86_64 ]; then
  NOMBRE_BINARIO="eslabon.exe"
else
  NOMBRE_BINARIO="eslabon"
fi

export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0
# eslabon: releases keep no debug info (see install.sh); lean on the machine.
export CARGO_PROFILE_RELEASE_DEBUG=none
export RUST_MIN_STACK=8388608

TMP="$(mktemp -d "${TMPDIR:-/tmp}/eslabon-release.XXXXXX")"

if [ "$ZIGBUILD" -eq 1 ]; then
  # eslabon: macOS `ar` corrompe objetos ELF; los wrappers de zig lo evitan.
  mkdir -p "$TMP/wrappers"
  printf '#!/bin/sh\nexec zig ar "$@"\n' > "$TMP/wrappers/ar"
  printf '#!/bin/sh\nexec zig ranlib "$@"\n' > "$TMP/wrappers/ranlib"
  chmod +x "$TMP/wrappers/ar" "$TMP/wrappers/ranlib"
  export AR="$TMP/wrappers/ar"
  export RANLIB="$TMP/wrappers/ranlib"
fi

echo "== building eslabon for $OBJETIVO${TRIPLE:+ ($TRIPLE)}"
cd "$RAIZ_REPO/codex-rs"
if [ "$ZIGBUILD" -eq 1 ]; then
  cargo zigbuild --release --locked -p codex-cli --bin eslabon --target "$TRIPLE"
elif [ -n "$TRIPLE" ]; then
  cargo build --release --locked -p codex-cli --bin eslabon --target "$TRIPLE"
else
  cargo build --release --locked -p codex-cli --bin eslabon
fi

BINARIO="$DIR_DESTINO/$NOMBRE_BINARIO"
[ -x "$BINARIO" ] || { echo "error: cargo did not produce $BINARIO" >&2; exit 1; }

limpiar() {
  rm -rf "$TMP"
}
trap limpiar EXIT INT TERM

if [ -z "$TRIPLE" ] && command -v strip >/dev/null 2>&1; then
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

VERSION="$(sed -n '/\[workspace.package\]/,/^\[/p' "$RAIZ_REPO/codex-rs/Cargo.toml" \
  | sed -n 's/^version = "\(.*\)"/\1/p' | head -1)"
if [ -z "$VERSION" ]; then
  echo "error: could not read the version from codex-rs/Cargo.toml" >&2
  exit 1
fi

DIR_PAQUETE="$TMP/eslabon-$VERSION-$OBJETIVO"
mkdir -p "$DIR_PAQUETE" "$DIR_DIST"
cp "$BINARIO" "$DIR_PAQUETE/$NOMBRE_BINARIO"
chmod +x "$DIR_PAQUETE/$NOMBRE_BINARIO"
cp "$RAIZ_REPO/LICENSE" "$RAIZ_REPO/NOTICE" "$RAIZ_REPO/README.md" "$DIR_PAQUETE/"

if [ "$OBJETIVO" = windows-x86_64 ]; then
  ARCHIVO="$DIR_DIST/eslabon-$VERSION-$OBJETIVO.zip"
  (cd "$DIR_PAQUETE" && zip -q "$ARCHIVO" "$NOMBRE_BINARIO" LICENSE NOTICE README.md)
else
  ARCHIVO="$DIR_DIST/eslabon-$VERSION-$OBJETIVO.tar.gz"
  tar -czf "$ARCHIVO" -C "$DIR_PAQUETE" "$NOMBRE_BINARIO" LICENSE NOTICE README.md
fi
(cd "$DIR_DIST" && hash_de_archivo "$(basename "$ARCHIVO")" > "$(basename "$ARCHIVO").sha256")

echo "== packaged $ARCHIVO"
cat "$ARCHIVO.sha256"

# eslabon: the acceptance proof is the packaged binary itself; extract it
# somewhere disposable and run the smoke harness against it.
DIR_HUMO="$TMP/humo"
mkdir -p "$DIR_HUMO"
cruzar=0
case "$OBJETIVO" in
  windows-*) cruzar=1 ;;
  macos-arm64) [ "$(uname -s)/$(uname -m)" = Darwin/arm64 ] || cruzar=1 ;;
  macos-x86_64) [ "$(uname -s)/$(uname -m)" = Darwin/x86_64 ] || cruzar=1 ;;
  linux-*) [ "$(uname -s)" = Linux ] || cruzar=1 ;;
esac
if [ "$cruzar" -eq 0 ]; then
  tar -xzf "$ARCHIVO" -C "$DIR_HUMO"
  if ! bash "$RAIZ_REPO/scripts/eslabon/humo/humo.sh" "$DIR_HUMO/$NOMBRE_BINARIO"; then
    echo "error: the packaged binary failed the smoke harness" >&2
    exit 1
  fi
else
  echo "release: $ARCHIVO (cross-compiled; smoke harness runs on the target or in CI)"
fi

echo "release: $ARCHIVO"
