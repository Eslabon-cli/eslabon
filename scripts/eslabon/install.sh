#!/bin/bash
# Compila eslabon optimizado e instala el comando `eslabon` en ~/.local/bin.
# No toca el `codex` oficial: eslabon usa su propio home (~/.eslabon).
#   bash scripts/eslabon/install.sh            # release sin LTO (rápido de compilar)
#   ESLABON_DEBUG=1 bash scripts/eslabon/install.sh   # usa el binario de desarrollo
#
# `eslabon` es un lanzador: ejecuta el binario con nombre `codex` (herdr y otros gestores
# de terminal reconocen al agente por argv[0]) y lo marca con ESLABON_LAUNCHER=1 para que
# se comporte como eslabon (app-server embebido, etc.).
set -euo pipefail
REPO="$(cd "$(dirname "$0")/../.." && pwd)/codex-rs"
DEST="${ESLABON_BIN_DIR:-$HOME/.local/bin}"
LIBEXEC="${ESLABON_LIBEXEC_DIR:-$HOME/.local/libexec/eslabon}"
mkdir -p "$DEST" "$LIBEXEC"
cd "$REPO"
if [ "${ESLABON_DEBUG:-0}" = "1" ]; then
  CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo build -p codex-cli
  ln -sfn "$REPO/target/debug/codex" "$LIBEXEC/codex"
else
  # Sin LTO ni símbolos: compila en minutos y el binario queda liviano.
  CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_DEBUG=none \
  CARGO_PROFILE_RELEASE_STRIP=symbols CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
    cargo build -p codex-cli --release
  # Copia a un nombre temporal y renombra: una sesión abierta conserva su binario.
  cp "$REPO/target/release/codex" "$LIBEXEC/codex.new"
  mv -f "$LIBEXEC/codex.new" "$LIBEXEC/codex"
fi
rm -f "$DEST/eslabon"
cat > "$DEST/eslabon" <<LAUNCHER
#!/bin/sh
# eslabon — lanzador generado por scripts/eslabon/install.sh
ESLABON_LAUNCHER=1 exec "$LIBEXEC/codex" "\$@"
LAUNCHER
chmod +x "$DEST/eslabon"
echo "Instalado: $DEST/eslabon -> $LIBEXEC/codex"
"$DEST/eslabon" --version
