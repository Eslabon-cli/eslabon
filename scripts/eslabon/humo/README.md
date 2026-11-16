# Arnés de humo de Eslabón

Verificación mínima y rápida del cable de Eslabón contra un proveedor OpenAI
falso en loopback, con un centinela que intercepta y rechaza cualquier intento
de salida hacia hosts que no sean `127.0.0.1`/`localhost`.

Es la base para validar los paquetes siguientes y cada merge de upstream: si el
arnés pasa, el binario arranca, habla el protocolo Chat Completions, ejecuta una
herramienta de shell y no se escapa a la red por su cuenta.

## Requisitos

- `bash`, `python3` (3.9+) y `git` en el `PATH`.
- El binario de desarrollo del repo. La única compilación permitida es:

```sh
cd codex-rs && CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 RUST_MIN_STACK=8388608 cargo build -p codex-cli
```

## Cómo correrlo

```sh
bash scripts/eslabon/humo/humo.sh                    # usa codex-rs/target/debug/codex
bash scripts/eslabon/humo/humo.sh /otro/codex        # binario alternativo
bash scripts/eslabon/humo/humo.sh --inventario      # no falla por el centinela
# eslabon entra en modo Eslabón por argv[0]: corre sin ESLABON_LAUNCHER.
bash scripts/eslabon/humo/humo.sh codex-rs/target/debug/eslabon
```

Salida esperada en una corrida limpia:

```
HUMO_CASO: texto
HUMO_CASO_OK: texto
HUMO_CASO: herramienta
HUMO_CASO_OK: herramienta
HUMO_HOSTS: (ninguno)
HUMO_OK
```

Con el binario `eslabon` la salida esperada termina en `HUMO_HOSTS: (ninguno)` y
`HUMO_OK`: el modo Eslabón apaga plugins y chequeos de red, y el nombre del
binario basta para entrar en él sin `ESLABON_LAUNCHER`.

Con `HUMO_OK` el código de salida es 0. Cualquier fallo imprime
`HUMO_FALLO: <caso>: <causa>` y devuelve 1 (o 124 si un caso agota los 90 s).

El script crea un home temporal con `mktemp -d`, escribe allí el `config.toml`,
arranca el proveedor falso y el centinela en puertos libres, y limpia todo al
salir (también si falla) con un `trap`.

## Qué comprueba cada caso

| Caso | Prompt | Qué prueba |
|---|---|---|
| `texto` | `HUMO_TEXTO` | Petición `POST /v1/chat/completions` con `stream: true`, SSE válido (incluido el chunk de `usage`) y la respuesta de texto llega a la sesión (`HUMO_TEXTO_OK`). |
| `herramienta` | `HUMO_HERRAMIENTA` | El binario acepta una llamada `tool_calls` a la herramienta de shell real (`exec_command`/`cmd`), la ejecuta y devuelve la salida (`echo HUMO_TOOL_RAN`) al modelo, que contesta `HUMO_HERRAMIENTA_OK`. |

El proveedor falso elige la herramienta leyendo el esquema real de `tools` en la
petición (la que tenga una propiedad `cmd`, `command` o `command_line`), no un
nombre inventado. También deja `usage` en el último chunk porque el cliente de
Chat espera ese chunk antes de `[DONE]`.

(El orden de `HUMO_HOSTS` varía según qué petición llegue primero; es la lista
de destinos únicos, no una secuencia.)

## El centinela y `--inventario`

`centinela.py` es un proxy HTTP/HTTPS (método `CONNECT`) en `127.0.0.1` que
**no** reenvía tráfico externo: responde `403` y anota cada intento no local en
un archivo JSON (host, puerto y momento). Los destinos `127.0.0.1`/`localhost` sí
se dejan pasar (túnel), aunque con `NO_PROXY=127.0.0.1,localhost` el cliente
nunca llega a usarlo para el proveedor.

`humo.sh` exporta `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY` (y sus variantes en
minúscula) hacia el centinela. **Atención:** esa configuración la respetan
también procesos externos, no solo Codex. El caso más importante es `git`: el
sincronizador de plugins curados lanza `git ls-remote`/`git fetch` contra
`github.com` en cada arranque y esas variables son la única forma de que el
centinela lo corte.

- Sin `--inventario`: cualquier intento registrado hace fallar la corrida con
  `HUMO_FALLO: centinela: <hosts>`.
- Con `--inventario`: los casos se comprueban igual, se imprime
  `HUMO_HOSTS: <hosts>` y la corrida sigue aunque haya intentos. Sirve para
  revisar qué se escapa hoy.

Lectura de la lista: en modo Eslabón los plugins, el chequeo de versión y la
analítica están apagados por defecto, así que una corrida limpia no muestra
destinos externos. Si aparece alguno, señala una fuga de red que hay que
revisar.

## Prueba por mutación

```sh
HUMO_ROMPER=herramienta bash scripts/eslabon/humo/humo.sh --inventario
```

Hace que el proveedor falso emita la llamada de herramienta con argumentos JSON
inválidos. El caso `herramienta` debe fallar (`HUMO_FALLO: herramienta: ...`):
prueba que el arnés detecta un tool call roto y no se lo traga como éxito.

## Limitaciones conocidas

- El centinela solo ve tráfico que respete las variables de proxy; un socket
  crudo (por ejemplo el WebSocket de control remoto, fila 22 de `red.md`) no
  pasa por aquí.
- No verifica el caso interactivo/TUI ni el arranque del app-server real: usa
  `codex exec`, que arranca un app-server en proceso.
- Dos casos con `exec_command` cubren el cable mínimo; no prueba streaming
  parcial letra a letra (el proveedor responde en un solo chunk).
