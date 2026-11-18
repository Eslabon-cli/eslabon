#!/bin/sh
# eslabon: public installer for the prebuilt `eslabon` releases (P7.2).
#
#   curl -fsSL https://github.com/Eslabon-cli/eslabon/raw/main/scripts/eslabon/install-release.sh | sh
#   ESLABON_VERSION=0.170.0 sh scripts/eslabon/install-release.sh
#
# Downloads the release archive for this machine (tar.gz on macOS/Linux, zip
# on Windows), verifies its SHA-256 against the published `.sha256` file and
# only then installs `eslabon` into
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
PLATAFORMA_WINDOWS=0

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
  MINGW*|MSYS*|CYGWIN*)
    OBJETIVO="windows-x86_64"
    PLATAFORMA_WINDOWS=1
    ;;
  *)
    fallar "unsupported system: $(uname -s)"
    ;;
esac

if [ "$PLATAFORMA_WINDOWS" -eq 1 ]; then
  EXTENSION="zip"
  BINARIO="eslabon.exe"
else
  EXTENSION="tar.gz"
  BINARIO="eslabon"
fi

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
if [ "$PLATAFORMA_WINDOWS" -eq 0 ]; then
  command -v tar >/dev/null 2>&1 || fallar "tar is required"
fi

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
  NOMBRE="eslabon-latest-$OBJETIVO.$EXTENSION"
  URL="$BASE_URL/latest/download/$NOMBRE"
else
  VERSION="${VERSION#v}"
  NOMBRE="eslabon-$VERSION-$OBJETIVO.$EXTENSION"
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
if [ "$PLATAFORMA_WINDOWS" -eq 0 ]; then
  tar -xzf "$TMP/$NOMBRE" -C "$DIR_BIN" eslabon ||
    fallar "could not extract $NOMBRE"
elif command -v unzip >/dev/null 2>&1; then
  unzip -q "$TMP/$NOMBRE" "$BINARIO" -d "$DIR_BIN" ||
    fallar "could not extract $NOMBRE"
elif command -v powershell.exe >/dev/null 2>&1 && command -v cygpath >/dev/null 2>&1; then
  powershell.exe -NoProfile -Command \
    "Expand-Archive -LiteralPath '$(cygpath -w "$TMP/$NOMBRE")' -DestinationPath '$(cygpath -w "$DIR_BIN")' -Force" ||
    fallar "could not extract $NOMBRE"
else
  fallar "need unzip or PowerShell to extract $NOMBRE"
fi
[ -x "$DIR_BIN/$BINARIO" ] || fallar "$NOMBRE does not contain an executable $BINARIO"

# eslabon: reuse the checksummed binary only after it proves it runs.
ESLABON_HOME="$TMP/home" CODEX_HOME="$TMP/home" \
  "$DIR_BIN/$BINARIO" --version </dev/null >"$TMP/version.txt" ||
  fallar "the downloaded eslabon binary does not run"
head -n 1 "$TMP/version.txt"

mkdir -p "$DESTINO"
cp "$DIR_BIN/$BINARIO" "$DESTINO/$BINARIO.nuevo"
chmod +x "$DESTINO/$BINARIO.nuevo"
mv -f "$DESTINO/$BINARIO.nuevo" "$DESTINO/$BINARIO"

case ":$PATH:" in
  *":$DESTINO:"*) ;;
  *)
    echo "warning: $DESTINO is not in PATH; add it to your shell profile to run eslabon" >&2
    ;;
esac

echo "installed: $DESTINO/$BINARIO"
ESLABON_HOME="$TMP/home" CODEX_HOME="$TMP/home" \
  "$DESTINO/$BINARIO" --version </dev/null | head -n 1
echo 'Next: run `eslabon init`.'
