#!/bin/bash
# Merge an upstream Codex release tag into the Eslabon tree and run the gate.
#
#   bash scripts/eslabon/upstream-sync.sh rust-v0.170.0
#   bash scripts/eslabon/upstream-sync.sh rust-v0.170.0 --dry-run
#   bash scripts/eslabon/upstream-sync.sh --continue
#   bash scripts/eslabon/upstream-sync.sh --update-baseline
#
# It never talks to the network except for the `git fetch` of the tag and it
# never pushes. Conflict resolution is manual: on conflicts the script groups
# the files by owner and exits 2 leaving the merge open.
#
# Resolution rule: in upstream files, take upstream and re-apply our minimal
# hook (the `// eslabon:` lines). Eslabon-owned files keep their behavior.

set -uo pipefail

RAIZ_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RUTA_BASELINE="$RAIZ_REPO/scripts/eslabon/upstream-baseline.txt"
RAMAL_BASE="${ESLABON_UPSTREAM_BASE:-publico}"
MIN_DISCO_GB=15
TAG=""
MODO_DRY_RUN=0
MODO_CONTINUAR=0
MODO_ACTUALIZAR=0

uso() {
  cat <<'FIN'
Uso: upstream-sync.sh <tag> [--dry-run | --continue]
     upstream-sync.sh --update-baseline

  <tag>               Tag de upstream a fusionar (ej. rust-v0.170.0).
  --dry-run           Comprueba el tag y simula el merge; no deja nada.
  --continue          Termina el merge abierto con los conflictos ya resueltos.
  --update-baseline   Reescribe scripts/eslabon/upstream-baseline.txt.
  --base <rama>       Rama base (por defecto publico, o ESLABON_UPSTREAM_BASE).
FIN
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run) MODO_DRY_RUN=1 ;;
    --continue) MODO_CONTINUAR=1 ;;
    --update-baseline) MODO_ACTUALIZAR=1 ;;
    --base) RAMAL_BASE="$2"; shift ;;
    -h|--help) uso; exit 0 ;;
    -*) echo "error: opción desconocida: $1" >&2; uso >&2; exit 1 ;;
    *) TAG="$1" ;;
  esac
  shift
done

if [ -z "$TAG" ] && [ "$MODO_CONTINUAR" -eq 0 ] && [ "$MODO_ACTUALIZAR" -eq 0 ]; then
  echo "error: falta el tag" >&2
  uso >&2
  exit 1
fi

log() { printf '%s\n' "$*"; }
fallo() { printf 'error: %s\n' "$*" >&2; exit 1; }

cd "$RAIZ_REPO" || fallo "no pude entrar a $RAIZ_REPO"
RAIZ_REPO="$(pwd -P)"

comprobar_disco() {
  libre_kb="$(df -Pk "$RAIZ_REPO" | awk 'NR==2 {print $4}')"
  [ -n "$libre_kb" ] || fallo "no pude leer el espacio libre"
  libre_gb=$((libre_kb / 1024 / 1024))
  if [ "$libre_gb" -lt "$MIN_DISCO_GB" ]; then
    fallo "disco libre en $RAIZ_REPO: ${libre_gb} GB; mínimo ${MIN_DISCO_GB} GB"
  fi
}

arbol_limpio() {
  if [ -n "$(git status --porcelain)" ]; then
    log "el árbol no está limpio:"
    git status --short >&2
    fallo "compromete o descarta tus cambios antes de sincronizar"
  fi
}

version_del_tag() { printf '%s\n' "${1#rust-v}"; }
rama_del_merge() { printf 'merge/%s\n' "$(version_del_tag "$1")"; }

asegurar_tag() {
  if git rev-parse --verify "refs/tags/$TAG" >/dev/null 2>&1; then
    return 0
  fi
  log "sin tag local $TAG; bajándolo de origin"
  # eslabon: el clon parcial rechaza `fetch origin tag <tag>` con
  # "upload-pack: not our ref"; se resuelve el objeto por SHA y se etiqueta.
  sha="$(git ls-remote --tags origin "refs/tags/$TAG" 2>/dev/null | awk 'NR==1 {print $1}')"
  [ -n "$sha" ] || fallo "el tag $TAG no existe en origin"
  git fetch --no-tags origin "$sha" || fallo "git fetch origin $sha falló"
  commit="$(git rev-parse "$sha^{commit}")" || fallo "no pude resolver $TAG a un commit"
  git tag -f "$TAG" "$commit" >/dev/null 2>&1 || true
  git rev-parse --verify "refs/tags/$TAG" >/dev/null 2>&1 \
    || fallo "el tag $TAG no apareció después del fetch"
}

clasificar_conflictos() {
  base="$(git merge-base HEAD MERGE_HEAD)"
  conflicto=""
  nuestros=""
  de_upstream=""
  conflicto="$(git diff --name-only --diff-filter=U | sort)"
  while IFS= read -r ruta; do
    [ -z "$ruta" ] && continue
    if git diff --quiet "$base" HEAD -- "$ruta"; then
      de_upstream="${de_upstream}${ruta}"$'\n'
    else
      nuestros="${nuestros}${ruta}"$'\n'
    fi
  done <<< "$conflicto"
}

imprimir_conflictos() {
  log "Conflictos sin resolver:"
  log "  nuestros archivos (Eslabon los tocó):"
  if [ -n "$nuestros" ]; then
    printf '%s' "$nuestros" | sed 's/^/    /'
  else
    log "    (ninguno)"
  fi
  log "  archivos de upstream:"
  if [ -n "$de_upstream" ]; then
    printf '%s' "$de_upstream" | sed 's/^/    /'
  else
    log "    (ninguno)"
  fi
  log ""
  log "Resuelve: en archivos de upstream toma upstream y reaplica el gancho"
  log "mínimo ('// eslabon:'); en los nuestros conserva el comportamiento."
  log "Marca con git add y relanza: $0 --continue"
}

medir() {
  # $1 = "crate/target" -> imprime "pasadas ignoradas fallidas"
  local entrada="$1"
  local crate="${entrada%%/*}"
  local destino="${entrada#*/}"
  local args=(test -p "$crate")
  local filtro=""
  case "$destino" in
    bin-*) args+=(--bin "${destino#bin-}") ;;
    lib:* ) args+=(--lib); filtro="${destino#lib:}" ;;
    lib ) args+=(--lib) ;;
    all ) : ;;
    *) echo "baseline: target desconocido: $destino" >&2; return 1 ;;
  esac
  if [ -n "$filtro" ]; then
    args+=("$filtro")
  fi
  local salida
  salida="$(cd "$RAIZ_REPO/codex-rs" \
    && env CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 RUST_MIN_STACK=8388608 \
       "${args[@]}" -- --test-threads=4 2>&1)" || true
  printf '%s\n' "$salida" > "/tmp/eslabon-test-$(printf '%s' "$entrada" | tr '/:' '-').log"
  local pasadas=0 ignoradas=0 fallidas=0
  while IFS= read -r linea; do
    case "$linea" in
      *"test result:"*)
        p="$(printf '%s' "$linea" | sed -n 's/.* \([0-9][0-9]*\) passed.*/\1/p')"
        i="$(printf '%s' "$linea" | sed -n 's/.* \([0-9][0-9]*\) ignored.*/\1/p')"
        f="$(printf '%s' "$linea" | sed -n 's/.* \([0-9][0-9]*\) failed.*/\1/p')"
        pasadas=$((pasadas + ${p:-0}))
        ignoradas=$((ignoradas + ${i:-0}))
        fallidas=$((fallidas + ${f:-0}))
        ;;
    esac
  done <<< "$salida"
  printf '%s %s %s\n' "$pasadas" "$ignoradas" "$fallidas"
}

leer_baseline() {
  ENTRAAS=""
  NUMEROS=""
  while IFS= read -r linea; do
    case "$linea" in
      \#*|"") continue ;;
    esac
    entrada="${linea%%[[:space:]]*}"
    numero="${linea##*[[:space:]]}"
    case "$numero" in
      *[!0-9]*) continue ;;
    esac
    ENTRAAS="${ENTRAAS}${entrada}"$'\n'
    NUMEROS="${NUMEROS}${numero}"$'\n'
  done < "$RUTA_BASELINE"
}

compuerta() {
  log "compuerta: build dev lean"
  if ! (cd "$RAIZ_REPO/codex-rs" \
        && env CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 RUST_MIN_STACK=8388608 \
           cargo build -p codex-cli) > /tmp/eslabon-build.log 2>&1; then
    tail -20 /tmp/eslabon-build.log
    return 1
  fi

  [ -f "$RUTA_BASELINE" ] || fallo "falta el baseline $RUTA_BASELINE (usa --update-baseline)"
  leer_baseline
  [ -n "$ENTRAAS" ] || fallo "el baseline no tiene líneas 'crate/target N'"

  local falla=0
  local detalle=""
  local i=0
  while IFS= read -r entrada; do
    [ -z "$entrada" ] && continue
    i=$((i + 1))
    esperado="$(printf '%s' "$NUMEROS" | sed -n "${i}p")"
    leido="$(medir "$entrada")" || { falla=1; detalle="${detalle}  FALLA ${entrada}: no se pudo medir
"; continue; }
    obtenido="$(printf '%s' "$leido" | cut -d' ' -f1)"
    ignorado="$(printf '%s' "$leido" | cut -d' ' -f2)"
    fallidas="$(printf '%s' "$leido" | cut -d' ' -f3)"
    if [ "$obtenido" -eq "$esperado" ] && [ "$fallidas" -eq 0 ]; then
      detalle="${detalle}  OK    ${entrada}: ${obtenido} (ignoradas ${ignorado})
"
    else
      detalle="${detalle}  FALLA ${entrada}: ${obtenido} de ${esperado}, ${fallidas} fallos
"
      falla=1
    fi
  done <<< "$ENTRAAS"

  log "compuerta: humo"
  if bash "$RAIZ_REPO/scripts/eslabon/humo/humo.sh" > /tmp/eslabon-humo.log 2>&1; then
    detalle="${detalle}  OK    humo.sh (HUMO_OK)
"
  else
    tail -20 /tmp/eslabon-humo.log
    detalle="${detalle}  FALLA humo.sh
"
    falla=1
  fi

  printf '%s' "$detalle"
  if [ "$falla" -eq 0 ]; then
    printf 'PASA\n'
    return 0
  fi
  printf 'FALLA\n'
  return 1
}

escribir_baseline() {
  log "midiendo el baseline en el árbol actual (puede tardar)"
  {
    printf '# Eslabon — baseline de prueba de upstream\n'
    printf '# Generado con scripts/eslabon/upstream-sync.sh --update-baseline\n'
    printf '# Formato: crate/target <pasadas>\n'
    printf '# Fallos conocidos que no cuentan como regresión:\n'
    printf '#   - daybreak en lote (batch)\n'
    printf '#   - packaged_daemon_bootstrap_seeds_local_package\n'
    printf '#   - snapshots de codex-tui por desfase de versión\n'
    printf '#   - codex-code-mode-host (5 fallos sin V8 precompilado)\n'
    for entrada in eslabon-config/all codex-cli/bin-codex codex-core/lib:config:: \
                   codex-tui/lib:eslabon codex-models-manager/all \
                   codex-http-client/all codex-exec/lib; do
      leido="$(medir "$entrada")"
      obtenido="$(printf '%s' "$leido" | cut -d' ' -f1)"
      [ -n "$obtenido" ] || fallo "no pude medir $entrada"
      printf '%s %s\n' "$entrada" "$obtenido"
    done
  } > "$RUTA_BASELINE.tmp"
  mv "$RUTA_BASELINE.tmp" "$RUTA_BASELINE"
  log "baseline escrito en $RUTA_BASELINE"
}

if [ "$MODO_ACTUALIZAR" -eq 1 ]; then
  arbol_limpio
  comprobar_disco
  escribir_baseline
  exit 0
fi

comprobar_disco

if [ "$MODO_CONTINUAR" -eq 1 ]; then
  if [ ! -f .git/MERGE_HEAD ]; then
    fallo "no hay merge en curso; usa upstream-sync.sh <tag>"
  fi
  if [ -z "$TAG" ]; then
    TAG="rust-v$(git show MERGE_HEAD:codex-rs/Cargo.toml \
      | sed -n '/\[workspace.package\]/,/^\[/p' \
      | sed -n 's/^version = "\(.*\)"/\1/p' | head -1)"
  fi
  if [ -n "$(git diff --name-only --diff-filter=U)" ]; then
    clasificar_conflictos
    imprimir_conflictos
    exit 2
  fi
  git add -A
  git commit --no-edit -m "Merge upstream $TAG" || fallo "no pude cerrar el merge"
  log "merge cerrado: $(git log --oneline -1)"
  compuerta || exit 1
  exit 0
fi

arbol_limpio
asegurar_tag
RAMA_MERGE="$(rama_del_merge "$TAG")"

if [ "$MODO_DRY_RUN" -eq 1 ]; then
  RAMA_ORIGEN="$(git rev-parse --abbrev-ref HEAD)"
  log "simulacro: merge de $TAG sobre $RAMA_ORIGEN"
  git checkout -q -b "simulacro/$TAG" || fallo "no pude crear la rama de simulación"
  git merge --no-commit --no-ff "$TAG" > /tmp/eslabon-dry-merge.log 2>&1
  estado=$?
  if [ "$estado" -ne 0 ]; then
    if [ -f .git/MERGE_HEAD ]; then
      clasificar_conflictos
      imprimir_conflictos
      git merge --abort >/dev/null 2>&1 || true
    else
      tail -20 /tmp/eslabon-dry-merge.log
      git checkout -- . 2>/dev/null || true
    fi
  else
    log "dry-run: el merge sería limpio"
  fi
  git checkout -q "$RAMA_ORIGEN"
  git branch -D "simulacro/$TAG" >/dev/null 2>&1 || true
  exit 0
fi

log "creando $RAMA_MERGE desde $RAMAL_BASE y fusionando $TAG"
actual="$(git rev-parse --abbrev-ref HEAD)"
if [ "$actual" != "$RAMAL_BASE" ]; then
  git rev-parse --verify "refs/heads/$RAMAL_BASE" >/dev/null 2>&1 \
    || fallo "rama base $RAMAL_BASE no existe"
  git checkout "$RAMAL_BASE" || fallo "no pude cambiar a $RAMAL_BASE"
fi
git checkout -q -b "$RAMA_MERGE" || fallo "no pude crear $RAMA_MERGE"
git merge --no-ff "$TAG" > /tmp/eslabon-merge.log 2>&1
estado=$?
if [ "$estado" -ne 0 ]; then
  if [ -f .git/MERGE_HEAD ]; then
    clasificar_conflictos
    imprimir_conflictos
    exit 2
  fi
  tail -30 /tmp/eslabon-merge.log
    git checkout -- . 2>/dev/null || true
    git checkout -q "$RAMAL_BASE" 2>/dev/null || true
    git branch -D "$RAMA_MERGE" >/dev/null 2>&1 || true
    fallo "el merge falló y no dejó un merge abierto (árbol restaurado)"
fi
log "merge limpio: $(git log --oneline -1)"
compuerta || exit 1
exit 0

