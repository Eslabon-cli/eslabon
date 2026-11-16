#!/bin/bash
# Arnés de humo propio de Eslabón: cable texto + herramienta contra un
# proveedor OpenAI falso en loopback, con un centinela que denuncia (y corta)
# cualquier intento de salida hacia hosts que no sean 127.0.0.1/localhost.
# El tercer caso, init, comprueba que `eslabon init` deja un home vacío que
# responde sin escribir el config.toml a mano.
#
#   bash scripts/eslabon/humo/humo.sh [ruta-al-binario] [--inventario]
#
# El binario por defecto es codex-rs/target/debug/codex del repo.  Termina con
# HUMO_OK (código 0) o HUMO_FALLO: <caso>: <causa> (código 1).
# Con --inventario el centinela no hace fallar la corrida: se listan los hosts
# intentados para revisar la auditoría de red.
set -uo pipefail

LIMITE_CASO=90
RAIZ_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
BINARIO="$RAIZ_REPO/codex-rs/target/debug/codex"
INVENTARIO=0

for argumento in "$@"; do
  case "$argumento" in
    --inventario) INVENTARIO=1 ;;
    --*) echo "HUMO_FALLO: argumento: desconocido $argumento" >&2; exit 1 ;;
    *) BINARIO="$argumento" ;;
  esac
done

DIR_TMP="$(mktemp -d "${TMPDIR:-/tmp}/humo-eslabon.XXXXXX")"
HOME_TMP="$DIR_TMP/home"
LOG_PETICIONES="$DIR_TMP/peticiones.jsonl"
LOG_CENTINELA="$DIR_TMP/centinela.jsonl"
mkdir -p "$HOME_TMP"
PID_PROVEEDOR=""
PID_CENTINELA=""

limpiar() {
  if [ -n "$PID_PROVEEDOR" ]; then
    kill "$PID_PROVEEDOR" 2>/dev/null
  fi
  if [ -n "$PID_CENTINELA" ]; then
    kill "$PID_CENTINELA" 2>/dev/null
  fi
  if [ -n "$DIR_TMP" ] && [ -d "$DIR_TMP" ]; then
    rm -rf "$DIR_TMP"
  fi
}
trap limpiar EXIT INT TERM

fallar() {
  echo "HUMO_FALLO: $*"
  exit 1
}

# Dos puertos libres distintos, reservados a la vez para que no se repita.
dos_puertos_libres() {
  python3 -c 'import socket
puertos = []
sockets = []
for _ in range(2):
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    puertos.append(s.getsockname()[1])
    sockets.append(s)
for s in sockets:
    s.close()
print(puertos[0], puertos[1])'
}

esperar_listo() {
  local archivo="$1"
  local marca="$2"
  local limite=10
  local inicio=$SECONDS
  while (( SECONDS - inicio < limite )); do
    if grep -Fq "$marca" "$archivo" 2>/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

# Corre un comando con límite fijo de segundos.  Devuelve 124 si venció.
ejecutar_con_limite() {
  local limite="$1"
  shift
  "$@" >"$SALIDA_CASO" 2>"$ERROR_CASO" </dev/null &
  local pid=$!
  local inicio=$SECONDS
  while kill -0 "$pid" 2>/dev/null; do
    if (( SECONDS - inicio >= limite )); then
      kill -TERM "$pid" 2>/dev/null
      local espera=0
      while kill -0 "$pid" 2>/dev/null && (( espera < 20 )); do
        sleep 0.1
        espera=$((espera + 1))
      done
      if kill -0 "$pid" 2>/dev/null; then
        kill -KILL "$pid" 2>/dev/null
      fi
      wait "$pid" 2>/dev/null
      return 124
    fi
    sleep 0.1
  done
  wait "$pid"
}

hosts_centinela() {
  python3 - "$LOG_CENTINELA" <<'PY'
import json
import sys

vistos = []
try:
    archivo = open(sys.argv[1], encoding="utf-8")
except OSError:
    print("")
    raise SystemExit(0)
for linea in archivo:
    linea = linea.strip()
    if not linea:
        continue
    try:
        registro = json.loads(linea)
    except ValueError:
        continue
    host = registro.get("host") or ""
    puerto = registro.get("puerto") or ""
    clave = "%s:%s" % (host, puerto)
    if host and clave not in vistos:
        vistos.append(clave)
print(" ".join(vistos))
PY
}

correr_caso() {
  local caso="$1"
  local prompt="$2"
  local esperado="$3"
  local lanzador=1
  # eslabon: a binary named `eslabon` enters the mode by argv[0]; exporting the
  # launcher marker here would hide a broken name detection.
  case "$(basename "$BINARIO")" in
    eslabon | eslabon.exe) lanzador="" ;;
  esac
  SALIDA_CASO="$DIR_TMP/salida-$caso.txt"
  ERROR_CASO="$DIR_TMP/error-$caso.txt"
  echo "HUMO_CASO: $caso"
  ejecutar_con_limite "$LIMITE_CASO" \
    env -u ESLABON_LAUNCHER -u ESLABON_HOME ${lanzador:+ESLABON_LAUNCHER=1} \
    CODEX_HOME="$HOME_TMP" "$BINARIO" \
    exec --skip-git-repo-check "$prompt"
  local codigo=$?
  if [ "$codigo" -eq 124 ]; then
    fallar "$caso: se agotó el límite de ${LIMITE_CASO} s"
  fi
  if [ "$codigo" -ne 0 ]; then
    fallar "$caso: el binario terminó con código $codigo"
  fi
  if ! grep -Fq "$esperado" "$SALIDA_CASO"; then
    fallar "$caso: la salida no contiene $esperado"
  fi
  echo "HUMO_CASO_OK: $caso"
}

# eslabon: un paso del caso «init», con el mismo límite y el mismo prefijo de
# fallo que el resto del caso (`init: <paso>: <causa>`).
paso_init() {
  local paso="$1"
  shift
  # eslabon: the case exercises ESLABON_HOME, the variable init creates.
  local -a entorno=(env -u ESLABON_LAUNCHER -u ESLABON_HOME -u CODEX_HOME)
  if [ -n "$LANZADOR_INIT" ]; then
    entorno+=("ESLABON_LAUNCHER=1")
  fi
  entorno+=("ESLABON_HOME=$HOME_INIT")
  local salida="$DIR_TMP/salida-init-$paso.txt"
  local error="$DIR_TMP/error-init-$paso.txt"
  SALIDA_CASO="$salida"
  ERROR_CASO="$error"
  ejecutar_con_limite "$LIMITE_CASO" \
    "${entorno[@]}" "$BINARIO" "$@"
  local codigo=$?
  if [ "$codigo" -eq 124 ]; then
    fallar "init: $paso: se agotó el límite de ${LIMITE_CASO} s"
  fi
  if [ "$codigo" -ne 0 ]; then
    local detalle
    detalle="$(tail -n 1 "$error" 2>/dev/null)"
    [ -n "$detalle" ] && detalle=" ($detalle)"
    fallar "init: $paso: el binario terminó con código $codigo$detalle"
  fi
}

# eslabon (P5.6): F5 pedía un home vacío donde `init` escribe el config y una
# sesión que responde; este caso lo corre sin tocar el config a mano.
# La aprobación y el sandbox van por `-c` en la línea de `exec`, no en el
# config.toml que escribe init.
correr_caso_init() {
  local lanzador=1
  # eslabon: la misma regla argv[0] que correr_caso; exportar el marcador con
  # un binario llamado `eslabon` ocultaría la detección del nombre.
  case "$(basename "$BINARIO")" in
    eslabon | eslabon.exe) lanzador="" ;;
  esac
  HOME_INIT="$DIR_TMP/home-init"
  echo "HUMO_CASO: init"
  LANZADOR_INIT="$lanzador"
  paso_init "init" init \
    --base-url "http://127.0.0.1:$PUERTO_PROVEEDOR/v1" --model humo-1
  # eslabon: init must create the home itself, with the owner-only mode.
  local permisos_home
  permisos_home="$(ls -ld "$HOME_INIT" 2>/dev/null | awk '{print $1}')"
  # macOS appends an `@` when the entry has extended attributes.
  permisos_home="${permisos_home%@}"
  if [ "$permisos_home" != "drwx------" ]; then
    fallar "init: home: permisos «$permisos_home» (se esperaba drwx------)"
  fi
  paso_init "revisar" revisar --home "$HOME_INIT"
  if ! grep -Fq "0 errores, 0 avisos" "$SALIDA_CASO"; then
    fallar "init: revisar: la salida no termina con «0 errores, 0 avisos»"
  fi
  paso_init "exec" exec --skip-git-repo-check \
    -c approval_policy=never -c sandbox_mode=danger-full-access \
    HUMO_HERRAMIENTA
  if ! grep -Fq "HUMO_HERRAMIENTA_OK" "$SALIDA_CASO"; then
    fallar "init: exec: la salida no contiene HUMO_HERRAMIENTA_OK"
  fi
  # eslabon: the unknown-model notice goes to stderr, so check the error file too.
  if grep -Fiq "Model metadata" "$SALIDA_CASO" "$ERROR_CASO" \
    || grep -Fiq "fallback" "$SALIDA_CASO" "$ERROR_CASO"; then
    fallar "init: exec: la salida contiene el aviso de modelo desconocido"
  fi
  unset HOME_INIT
  echo "HUMO_CASO_OK: init"
}

[ -x "$BINARIO" ] || fallar "binario: no existe o no es ejecutable $BINARIO"

PUERTOS="$(dos_puertos_libres)" || { fallar "puertos: no se pudieron reservar puertos libres"; }
read -r PUERTO_PROVEEDOR PUERTO_CENTINELA <<<"$PUERTOS"
if [ -z "$PUERTO_PROVEEDOR" ] || [ -z "$PUERTO_CENTINELA" ] || [ "$PUERTO_PROVEEDOR" = "$PUERTO_CENTINELA" ]; then
  fallar "puertos: no se pudieron reservar dos puertos libres distintos"
fi

python3 "$RAIZ_REPO/scripts/eslabon/humo/proveedor_falso.py" \
  --port "$PUERTO_PROVEEDOR" --log "$LOG_PETICIONES" >"$DIR_TMP/proveedor.out" 2>&1 &
PID_PROVEEDOR=$!
python3 "$RAIZ_REPO/scripts/eslabon/humo/centinela.py" \
  --port "$PUERTO_CENTINELA" --log "$LOG_CENTINELA" >"$DIR_TMP/centinela.out" 2>&1 &
PID_CENTINELA=$!

esperar_listo "$DIR_TMP/proveedor.out" "PROVEEDOR_LISTO $PUERTO_PROVEEDOR" ||
  fallar "proveedor: no arrancó el proveedor falso ($(tail -n 3 "$DIR_TMP/proveedor.out" 2>/dev/null | tr '\n' ' '))"
esperar_listo "$DIR_TMP/centinela.out" "CENTINELA_LISTO $PUERTO_CENTINELA" ||
  fallar "centinela: no arrancó el centinela ($(tail -n 3 "$DIR_TMP/centinela.out" 2>/dev/null | tr '\n' ' '))"

cat >"$HOME_TMP/config.toml" <<TOML
model = "humo-1"
model_provider = "humo"
approval_policy = "never"
sandbox_mode = "danger-full-access"

[model_providers.humo]
name = "Proveedor de humo en loopback"
base_url = "http://127.0.0.1:$PUERTO_PROVEEDOR/v1"
wire_api = "chat"
requires_openai_auth = false
TOML

export HTTPS_PROXY="http://127.0.0.1:$PUERTO_CENTINELA"
export HTTP_PROXY="$HTTPS_PROXY"
export ALL_PROXY="$HTTPS_PROXY"
export https_proxy="$HTTPS_PROXY"
export http_proxy="$HTTPS_PROXY"
export all_proxy="$HTTPS_PROXY"
export NO_PROXY="127.0.0.1,localhost"
export no_proxy="$NO_PROXY"

correr_caso "texto" "HUMO_TEXTO" "HUMO_TEXTO_OK"
correr_caso "herramienta" "HUMO_HERRAMIENTA" "HUMO_HERRAMIENTA_OK"
correr_caso_init

HOSTS="$(hosts_centinela)"

if [ -n "$HOSTS" ]; then
  if [ "$INVENTARIO" -eq 1 ]; then
    echo "HUMO_HOSTS: $HOSTS"
  else
    fallar "centinela: $HOSTS"
  fi
else
  echo "HUMO_HOSTS: (ninguno)"
fi

echo "HUMO_OK"
exit 0
