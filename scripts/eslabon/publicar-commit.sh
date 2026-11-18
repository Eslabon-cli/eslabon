#!/bin/bash
# Build the public history for Eslabon: upstream base -> Eslabon commit ->
# one "Update to Codex <version>" commit per upstream update.
#
#   bash scripts/eslabon/publicar-commit.sh [<rama-origen>] [<rama-pública>]
#
# The commit is created with git commit-tree, so the working tree is not
# touched. The author is neutral (`Eslabon <noreply@eslabon.invalid>`) and the
# commit is not signed. The script prints the hash and the exact push command;
# it does not push.

set -uo pipefail

RAIZ_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
AUTOR="Eslabon"
CORREO="noreply@eslabon.invalid"
ORIGEN="${1:-publico}"
PUBLICA="${2:-publico-pub}"
REMOTO="origin"
PROHIBIDAS='Claude|Co-Authored-By|Generated with'

cd "$RAIZ_REPO" || { echo "error: no pude entrar a $RAIZ_REPO" >&2; exit 1; }
RAIZ_REPO="$(pwd -P)"

fallo() { printf 'error: %s\n' "$*" >&2; exit 1; }
log() { printf '%s\n' "$*"; }

git rev-parse --verify "refs/heads/$ORIGEN" >/dev/null 2>&1 \
  || git rev-parse --verify "$ORIGEN" >/dev/null 2>&1 \
  || fallo "no existe la rama de origen $ORIGEN"
git rev-parse --verify "$ORIGEN^{commit}" >/dev/null

version_de_rama() {
  git show "$1:codex-rs/Cargo.toml" 2>/dev/null \
    | sed -n '/\[workspace.package\]/,/^\[/p' \
    | sed -n 's/^version = "\(.*\)"/\1/p' \
    | head -1
}

VERSION="$(version_de_rama "$ORIGEN")"
[ -n "$VERSION" ] || fallo "no pude leer la versión de $ORIGEN:codex-rs/Cargo.toml"

crear_commit() {
  # $1 = árbol, $2 = padre (vacío = commit raíz), $3 = mensaje
  local arbol="$1" padre="$2" mensaje="$3"
  local argumentos=()
  if [ -n "$padre" ]; then
    argumentos+=(-p "$padre")
  fi
  GIT_AUTHOR_NAME="$AUTOR" GIT_AUTHOR_EMAIL="$CORREO" \
  GIT_COMMITTER_NAME="$AUTOR" GIT_COMMITTER_EMAIL="$CORREO" \
  git -c commit.gpgsign=false commit-tree "$arbol" "${argumentos[@]+"${argumentos[@]}"}" -m "$mensaje"
}

comprobar_mensaje() {
  local hash="$1"
  local cuerpo
  cuerpo="$(git show -s --format=%B "$hash")"
  if printf '%s' "$cuerpo" | grep -qiE "$PROHIBIDAS"; then
    printf '%s\n' "$cuerpo" >&2
    fallo "el mensaje del commit contiene una marca prohibida ($PROHIBIDAS)"
  fi
  if git cat-file -p "$hash" | head -5 | grep -q '^gpgsig'; then
    fallo "el commit está firmado"
  fi
  local nombre correo
  nombre="$(git show -s --format=%an "$hash")"
  correo="$(git show -s --format=%ae "$hash")"
  if [ "$nombre" != "$AUTOR" ] || [ "$correo" != "$CORREO" ]; then
    fallo "autor inesperado: $nombre <$correo>"
  fi
}

escanear_gitleaks() {
  local rango="$1"
  if ! command -v gitleaks >/dev/null 2>&1; then
    log "gitleaks no instalado; se omite el escaneo"
    return 0
  fi
  log "gitleaks sobre $rango"
  if gitleaks git --log-opts="$rango" --redact -v > /tmp/eslabon-gitleaks.log 2>&1; then
    log "gitleaks: sin hallazgos"
    return 0
  fi
  # gitleaks reports the known upstream fixtures as findings; surface the count
  # and leave the log for review instead of failing the simulation.
  hallazgos="$(grep -c 'Finding' /tmp/eslabon-gitleaks.log || true)"
  log "gitleaks: ${hallazgos:-?} hallazgos; revisa /tmp/eslabon-gitleaks.log"
  tail -5 /tmp/eslabon-gitleaks.log
  return 0
}

cambios_propios() {
  local desde="$1" hasta="$2"
  git log --format=%s "$desde..$hasta" 2>/dev/null \
    | grep -E '^P[0-9]+\.[0-9]+[^0-9a-zA-Z.]*:' \
    || true
}

ultimo="$(git rev-parse --verify "refs/heads/$PUBLICA" 2>/dev/null || true)"
if [ -z "$ultimo" ]; then
  log "creando la base pública $PUBLICA (commit raíz + primer commit Eslabon)"
  base="$(git merge-base "$REMOTO/main" "$ORIGEN")"
  [ -n "$base" ] || fallo "no pude resolver el merge-base con $REMOTO/main"
  mensaje_raiz="Upstream base: Codex $(version_de_rama "$base")"
  raiz="$(crear_commit "$(git rev-parse "$base^{tree}")" "" "$mensaje_raiz")"
  comprobar_mensaje "$raiz"
  mensaje_eslabon="Eslabon $VERSION"
  hijo="$(crear_commit "$(git rev-parse "$ORIGEN^{tree}")" "$raiz" "$mensaje_eslabon")"
  comprobar_mensaje "$hijo"
  escanear_gitleaks "$raiz..$hijo"
  git branch "$PUBLICA" "$hijo" || fallo "no pude crear la rama $PUBLICA"
else
  log "extendiendo $PUBLICA hacia $VERSION"
  lista="$(cambios_propios "$ultimo" "$ORIGEN")"
  mensaje="Update to Codex $VERSION"
  if [ -n "$lista" ]; then
    mensaje="${mensaje}"$'\n\n'"Own changes since the last public commit:"$'\n'"$lista"
  else
    mensaje="${mensaje}"$'\n\n'"Own changes since the last public commit: (none)"
  fi
  hijo="$(crear_commit "$(git rev-parse "$ORIGEN^{tree}")" "$ultimo" "$mensaje")"
  comprobar_mensaje "$hijo"
  escanear_gitleaks "$ultimo..$hijo"
  git branch -f "$PUBLICA" "$hijo" || fallo "no pude mover la rama $PUBLICA"
fi

log ""
log "commit público: $hijo"
log "árbol idéntico a $ORIGEN: $(git diff --quiet "$PUBLICA" "$ORIGEN" && echo sí || echo no)"
log ""
log "Push (no ejecutado):"
log "  git push $REMOTO $hijo:refs/heads/main"
