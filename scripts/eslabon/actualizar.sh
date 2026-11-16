#!/bin/bash
# Actualiza eslabon a una versión nueva de openai/codex (upstream) y lo instala.
#
#   bash scripts/eslabon/actualizar.sh               # última versión estable de upstream
#   bash scripts/eslabon/actualizar.sh rust-v0.163.0 # una etiqueta concreta
#
# Por qué no basta con "eslabon update": eslabon es un fork. El aviso de actualización viene del código
# de Codex y apunta al binario oficial; instalarlo borraría lo propio de eslabon (wire chat, PHI,
# catálogos, barra lateral, estado, revisar/desatendido). Hay que FUSIONAR la versión nueva en la rama
# de eslabon, compilar e instalar nuestro binario.
#
# Pasos: (1) trae las etiquetas de upstream; (2) fusiona la etiqueta en la rama de trabajo del worktree
# ~/testing/eslabon-up (nunca rebase); (3) si hay conflictos, se detiene sin tocar nada instalado;
# (4) respalda el binario instalado; (5) compila en release sin LTO con prioridad baja e instala con
# scripts/eslabon/install.sh (copia y renombra: las sesiones abiertas conservan su binario);
# (6) comprueba versión y configuración. La rama principal `eslabon` del checkout ~/testing/eslabon NO se
# mueve: al final se imprime el comando para avanzarla cuando nadie esté trabajando en ella.
set -euo pipefail

RAMA_TRABAJO="${ESLABON_RAMA:-eslabon-upstream}"
WT="${ESLABON_WORKTREE:-$HOME/testing/eslabon-up}"
REPO="${ESLABON_REPO:-$HOME/testing/eslabon}"
LIBEXEC="$HOME/.local/libexec/eslabon"
JOBS="${ESLABON_JOBS:-3}"

cd "$REPO"
echo "== 1. trayendo etiquetas de upstream"
git fetch -q origin --tags

ETIQUETA="${1:-}"
if [ -z "$ETIQUETA" ]; then
  ETIQUETA="$(gh release list -R openai/codex -L 20 --exclude-pre-releases --json tagName -q '.[0].tagName')"
fi
git rev-parse -q --verify "refs/tags/$ETIQUETA" >/dev/null || { echo "no existe la etiqueta $ETIQUETA"; exit 1; }
echo "   versión objetivo: $ETIQUETA (instalada: $("$LIBEXEC/codex" --version 2>/dev/null || echo ninguna))"

echo "== 2. worktree $WT"
if [ ! -d "$WT" ]; then
  git worktree add -q -b "$RAMA_TRABAJO" "$WT" eslabon
fi
cd "$WT"
if [ -n "$(git status --porcelain)" ]; then echo "el worktree tiene cambios sin commitear; resuélvelos antes"; exit 1; fi
if git merge-base --is-ancestor "$ETIQUETA" HEAD; then
  echo "   $ETIQUETA ya está fusionada en $(git branch --show-current)"
else
  echo "== 3. fusionando $ETIQUETA (sin rebase)"
  if ! git merge --no-ff "$ETIQUETA" -m "Merge upstream $ETIQUETA into eslabon"; then
    # Conflicto conocido y repetido: la línea `version = "…"` de [workspace.package] en codex-rs/Cargo.toml
    # (cada versión de upstream la cambia). Si es el ÚNICO conflicto y solo difiere esa línea, gana upstream.
    if [ "$(git diff --name-only --diff-filter=U)" = "codex-rs/Cargo.toml" ] && python3 - <<'PY'
import re,sys
p="codex-rs/Cargo.toml"; s=open(p).read()
pat=re.compile(r'<<<<<<< [^\n]*\n(version = "[^"\n]*"\n)=======\n(version = "[^"\n]*"\n)>>>>>>> [^\n]*\n')
n=len(re.findall(r'<<<<<<< ',s)); s2,k=pat.subn(lambda m:m.group(2),s)
if n==0 or k!=n: sys.exit(1)
open(p,"w").write(s2)
PY
    then
      git add codex-rs/Cargo.toml && git commit -q --no-edit && echo "   conflicto de versión en Cargo.toml resuelto a favor de upstream"
    else
      echo "!! conflictos: resuélvelos en $WT (git status), haz commit y vuelve a correr este script."
      echo "   No se instaló nada. Para abandonar: git -C $WT merge --abort"
      exit 2
    fi
  fi
  if [ -n "$(git status --porcelain codex-rs/Cargo.lock)" ]; then
    git add codex-rs/Cargo.lock && git commit -q -m "Cargo.lock tras el merge de $ETIQUETA"
  fi
fi

echo "== 4. respaldo del binario instalado"
if [ -f "$LIBEXEC/codex" ]; then
  SHA="$(shasum -a 256 "$LIBEXEC/codex" | cut -c1-8)"
  cp -p "$LIBEXEC/codex" "$LIBEXEC/codex.respaldo-$SHA"
  echo "   $LIBEXEC/codex.respaldo-$SHA   (volver atrás: mv ese archivo a $LIBEXEC/codex)"
fi

echo "== 5. compilando e instalando (release sin LTO, nice, $JOBS trabajos; ~30 min)"
nice -n 19 env CARGO_BUILD_JOBS="$JOBS" bash scripts/eslabon/install.sh

echo "== 6. comprobaciones"
"$HOME/.local/bin/eslabon" --version
"$HOME/.local/bin/eslabon" revisar --home "$HOME/.eslabon" | tail -1 || true

echo
echo "Listo. Pendiente (manual, cuando nadie trabaje en $REPO):"
echo "  git -C $REPO merge --ff-only $RAMA_TRABAJO     # avanza la rama principal eslabon"
echo "  y publicar el snapshot en el repo privado siulynot/eslabon (ver la nota del vault)."
