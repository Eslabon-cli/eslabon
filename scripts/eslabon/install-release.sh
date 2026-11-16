#!/bin/sh
# eslabon: public installer for the prebuilt `eslabon` releases (P7.2).
#
#   curl -fsSL https://github.com/Eslabon-cli/eslabon/raw/main/scripts/eslabon/install-release.sh | sh
#   ESLABON_VERSION=0.167.2 sh scripts/eslabon/install-release.sh
#
# Downloads the release tarball for this machine, verifies its SHA-256 against
# the published `.sha256` file and only then installs `eslabon` into
# ${ESLABON_INSTALL_DIR:-$HOME/.local/bin}. No sudo, no shell files touched.
#
#   ESLABON_VERSION     release to install; default is the latest one
#   ESLABON_INSTALL_DIR where to put the binary; default ~/.local/bin
#   ESLABON_BASE_URL    release base; default the GitHub releases page. A
#                       custom base serves `latest/download/eslabon-latest-<target>.tar.gz`
#                       so it can be tested against a local HTTP server.
set -eu

BASE_POR_DEFECTO="https://github.com/Eslabon-cli/eslabon/releases"
API_POR_DEFECTO="https://api.github.com/repos/Eslabon-cli/eslabon/releases/latest"
BASE_URL="${ESLABON_BASE_URL:-$BASE_POR_DEFECTO}"
VERSION="${ESLABON_VERSION:-}"
DESTINO="${ESLABON_INSTALL_DIR:-$HOME/.local/bin}"

fallar() {
  echo "error: $*" >&2
  exit 1
}

case "$(uname -s)" in
  Darwin)
    case "$(uname -m)" in
      arm64|aarch64) OBJETIVO="macos-arm64" ;;
      x86_64) OBJETIVO="macos-x86_64" ;;
      *) fallar "unsupported architecture: $(uname -m)" ;;
    esac
    ;;
  Linux)
    case "$(uname -m)" in
      x86_64|amd64) OBJETIVO="linux-x86_64" ;;
      aarch64|arm64) OBJETIVO="linux-arm64" ;;
      *) fallar "unsupported architecture: $(uname -m)" ;;
    esac
    ;;
  *)
    fallar "unsupported system: $(uname -s)"
    ;;
esac

hash_de_archivo() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1"
  else
    fallar "need sha256sum or shasum to verify the download"
  fi
}

command -v curl >/dev/null 2>&1 || fallar "curl is required"
command -v tar >/dev/null 2>&1 || fallar "tar is required"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/eslabon-install.XXXXXX")" ||
  fallar "could not create a temporary directory"
limpiar() {
  rm -rf "$TMP"
}
trap limpiar EXIT INT TERM

if [ -z "$VERSION" ] || [ "$VERSION" = "latest" ]; then
  case "$BASE_URL" in
    "$BASE_POR_DEFECTO")
      # eslabon: the asset name carries its version, so the latest tag is
      # resolved from the public releases API with a single, anonymous GET.
      TAG="$(curl -fsSL -H "User-Agent: eslabon-install" "$API_POR_DEFECTO" \
        | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
      [ -n "$TAG" ] || fallar "could not read the latest release tag from $API_POR_DEFECTO"
      VERSION="${TAG#v}"
      ;;
    *)
      # A custom base (local server, mirror) uses a literal `latest` asset name.
      VERSION="latest"
      ;;
  esac
fi

if [ "$VERSION" = "latest" ]; then
  NOMBRE="eslabon-latest-$OBJETIVO.tar.gz"
  URL="$BASE_URL/latest/download/$NOMBRE"
else
  VERSION="${VERSION#v}"
  NOMBRE="eslabon-$VERSION-$OBJETIVO.tar.gz"
  URL="$BASE_URL/download/v$VERSION/$NOMBRE"
fi

echo "downloading $URL"
curl -fSL --retry 2 -o "$TMP/$NOMBRE" "$URL" ||
  fallar "download failed: $URL"
curl -fSL --retry 2 -o "$TMP/$NOMBRE.sha256" "$URL.sha256" ||
  fallar "download failed: $URL.sha256 (the checksum file is required)"

ESPERADO="$(awk '{print $1}' "$TMP/$NOMBRE.sha256")"
[ -n "$ESPERADO" ] || fallar "the checksum file is empty: $URL.sha256"
OBTENIDO="$(hash_de_archivo "$TMP/$NOMBRE" | awk '{print $1}')"
if [ "$OBTENIDO" != "$ESPERADO" ]; then
  fallar "checksum mismatch for $NOMBRE: expected $ESPERADO, got $OBTENIDO"
fi

DIR_BIN="$TMP/bin"
mkdir -p "$DIR_BIN"
tar -xzf "$TMP/$NOMBRE" -C "$DIR_BIN" eslabon ||
  fallar "could not extract $NOMBRE"
[ -x "$DIR_BIN/eslabon" ] || fallar "$NOMBRE does not contain an executable eslabon"

# eslabon: reuse the checksummed binary only after it proves it runs.
ESLABON_HOME="$TMP/home" CODEX_HOME="$TMP/home" \
  "$DIR_BIN/eslabon" --version </dev/null >"$TMP/version.txt" ||
  fallar "the downloaded eslabon binary does not run"
head -n 1 "$TMP/version.txt"

mkdir -p "$DESTINO"
cp "$DIR_BIN/eslabon" "$DESTINO/eslabon.nuevo"
chmod +x "$DESTINO/eslabon.nuevo"
mv -f "$DESTINO/eslabon.nuevo" "$DESTINO/eslabon"

case ":$PATH:" in
  *":$DESTINO:"*) ;;
  *)
    echo "warning: $DESTINO is not in PATH; add it to your shell profile to run eslabon"
    ;;
esac

echo "installed: $DESTINO/eslabon"
ESLABON_HOME="$TMP/home" CODEX_HOME="$TMP/home" \
  "$DESTINO/eslabon" --version </dev/null | head -n 1
echo 'Next: run `eslabon init`.'
