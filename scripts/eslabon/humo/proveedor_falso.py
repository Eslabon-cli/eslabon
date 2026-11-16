#!/usr/bin/env python3
"""Proveedor OpenAI falso (Chat Completions) para el arnés de humo de Eslabon.

Escucha solo en 127.0.0.1 y responde de forma guionizada para que el arnés pueda
comprobar el cable (texto y llamada de herramienta) sin depender de la red ni de
un modelo real. Solo biblioteca estándar, Python 3.9+.

    python3 proveedor_falso.py --port 8099 --log peticiones.jsonl

Si HUMO_ROMPER=herramienta, la llamada de herramienta sale con argumentos JSON
inválidos a propósito: es la prueba por mutación del revisor.
"""

import argparse
import json
import os
import threading
import uuid
from datetime import datetime
from http.server import BaseHTTPRequestHandler
from http.server import ThreadingHTTPServer

MODELO = "humo-1"
MARCA_TEXTO = "HUMO_TEXTO"
MARCA_HERRAMIENTA = "HUMO_HERRAMIENTA"
TEXTO_OK = "HUMO_TEXTO_OK"
HERRAMIENTA_OK = "HUMO_HERRAMIENTA_OK"
COMANDO = "echo HUMO_TOOL_RAN"
MARCA_TOOL_RAN = "HUMO_TOOL_RAN"

CERROJO_LOG = threading.Lock()
ARCHIVO_LOG = None


def registrar_linea(registro):
    if ARCHIVO_LOG is None:
        return
    linea = json.dumps(registro, ensure_ascii=False, sort_keys=True)
    with CERROJO_LOG:
        with open(ARCHIVO_LOG, "a", encoding="utf-8") as archivo:
            archivo.write(linea + "\n")
            archivo.flush()


def texto_de(mensaje):
    """Devuelve el texto plano de un mensaje, sea string o lista de partes."""
    contenido = mensaje.get("content")
    if isinstance(contenido, str):
        return contenido
    if isinstance(contenido, list):
        partes = []
        for parte in contenido:
            if isinstance(parte, dict) and isinstance(parte.get("text"), str):
                partes.append(parte["text"])
        return "\n".join(partes)
    return ""


def herramienta_de_shell(herramientas):
    """Busca en `tools` la función que acepta un comando y devuelve (nombre, args)."""
    for herramienta in herramientas or []:
        if not isinstance(herramienta, dict):
            continue
        if herramienta.get("type") not in (None, "function"):
            continue
        funcion = herramienta.get("function") or {}
        nombre = funcion.get("name")
        if not nombre:
            continue
        esquema = funcion.get("parameters") or {}
        propiedades = esquema.get("properties") or {}
        for clave in ("cmd", "command", "command_line"):
            if clave in propiedades:
                if os.environ.get("HUMO_ROMPER") == "herramienta":
                    # JSON incompleto a propósito: el manejador debe rechazarlo.
                    return nombre, '{"' + clave + '": '
                return nombre, json.dumps({clave: COMANDO}, ensure_ascii=False)
    return "exec_command", json.dumps({"cmd": COMANDO}, ensure_ascii=False)


def guion(mensajes, peticion):
    """Decide la respuesta según el último mensaje de usuario y el estado del turno."""
    salidas = [texto_de(m) for m in mensajes if m.get("role") == "tool"]
    ya_corrio = any(MARCA_TOOL_RAN in salida for salida in salidas)

    pista = ""
    for mensaje in mensajes:
        if mensaje.get("role") != "user":
            continue
        texto = texto_de(mensaje)
        if MARCA_HERRAMIENTA in texto or MARCA_TEXTO in texto:
            pista = texto

    if MARCA_HERRAMIENTA in pista:
        if ya_corrio:
            return {"tipo": "texto", "texto": HERRAMIENTA_OK}
        if salidas:
            detalle = salidas[-1].strip().splitlines()
            detalle = detalle[0] if detalle else ""
            return {
                "tipo": "texto",
                "texto": "HUMO_HERRAMIENTA_SIN_EJECUTAR: " + detalle,
            }
        nombre, argumentos = herramienta_de_shell(peticion.get("tools"))
        return {"tipo": "herramienta", "nombre": nombre, "argumentos": argumentos}

    if MARCA_TEXTO in pista:
        return {"tipo": "texto", "texto": TEXTO_OK}

    return {"tipo": "texto", "texto": "HUMO_SIN_GUION: " + pista[:80]}


class Manejador(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "humo-falso/1"

    def log_message(self, formato, *args):  # silencia el log de stderr
        pass

    def do_GET(self):
        ruta = self.path.split("?")[0].rstrip("/") or "/"
        registrar_linea(
            {
                "momento": datetime.now().isoformat(timespec="seconds"),
                "metodo": "GET",
                "ruta": self.path,
                "modelo": None,
                "mensajes": 0,
                "tools": [],
            }
        )
        if ruta in ("/v1/models", "/models"):
            cuerpo = json.dumps(
                {
                    "object": "list",
                    "data": [
                        {
                            "id": MODELO,
                            "object": "model",
                            "created": 0,
                            "owned_by": "humo",
                        }
                    ],
                }
            ).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(cuerpo)))
            self.end_headers()
            self.wfile.write(cuerpo)
            return
        self.send_error(404, "ruta no guionizada")

    def do_POST(self):
        longitud = int(self.headers.get("Content-Length") or 0)
        crudo = self.rfile.read(longitud) if longitud > 0 else b""
        try:
            peticion = json.loads(crudo.decode("utf-8") or "{}")
        except ValueError:
            self.send_error(400, "json invalido")
            return
        if not isinstance(peticion, dict):
            self.send_error(400, "json no es un objeto")
            return
        if self.path.split("?")[0].rstrip("/") not in ("/v1/chat/completions", "/chat/completions"):
            self.send_error(404, "ruta no guionizada")
            return

        mensajes = peticion.get("messages") or []
        nombres_tools = []
        for herramienta in peticion.get("tools") or []:
            if isinstance(herramienta, dict):
                funcion = herramienta.get("function") or {}
                if funcion.get("name"):
                    nombres_tools.append(funcion["name"])
        registrar_linea(
            {
                "momento": datetime.now().isoformat(timespec="seconds"),
                "metodo": "POST",
                "ruta": self.path,
                "modelo": peticion.get("model"),
                "mensajes": len(mensajes),
                "tools": nombres_tools,
            }
        )

        accion = guion(mensajes, peticion)
        if peticion.get("stream"):
            self.responder_stream(accion)
        else:
            self.responder_json(accion)

    def responder_json(self, accion):
        mensaje = {"role": "assistant", "content": accion.get("texto")}
        if accion["tipo"] == "herramienta":
            mensaje = {
                "role": "assistant",
                "content": None,
                "tool_calls": [
                    {
                        "id": "call_humo_1",
                        "index": 0,
                        "type": "function",
                        "function": {
                            "name": accion["nombre"],
                            "arguments": accion["argumentos"],
                        },
                    }
                ],
            }
        cuerpo_respuesta = {
            "id": "chatcmpl-humo",
            "object": "chat.completion",
            "created": 0,
            "model": MODELO,
            "choices": [
                {
                    "index": 0,
                    "message": mensaje,
                    "finish_reason": "stop",
                }
            ],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15,
                "prompt_tokens_details": {"cached_tokens": 0},
            },
        }
        cuerpo = json.dumps(cuerpo_respuesta).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(cuerpo)))
        self.end_headers()
        self.wfile.write(cuerpo)

    def responder_stream(self, accion):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True
        id_respuesta = "chatcmpl-humo-" + uuid.uuid4().hex[:8]
        try:
            if accion["tipo"] == "texto":
                self.enviar_evento(
                    {
                        "id": id_respuesta,
                        "object": "chat.completion.chunk",
                        "created": 0,
                        "model": MODELO,
                        "choices": [
                            {
                                "index": 0,
                                "delta": {
                                    "role": "assistant",
                                    "content": accion["texto"],
                                },
                                "finish_reason": "stop",
                            }
                        ],
                    }
                )
            else:
                self.enviar_evento(
                    {
                        "id": id_respuesta,
                        "object": "chat.completion.chunk",
                        "created": 0,
                        "model": MODELO,
                        "choices": [
                            {
                                "index": 0,
                                "delta": {
                                    "role": "assistant",
                                    "tool_calls": [
                                        {
                                            "index": 0,
                                            "id": "call_humo_1",
                                            "type": "function",
                                            "function": {
                                                "name": accion["nombre"],
                                                "arguments": accion["argumentos"],
                                            },
                                        }
                                    ],
                                },
                                "finish_reason": "stop",
                            }
                        ],
                    }
                )
            self.enviar_evento(
                {
                    "id": id_respuesta,
                    "object": "chat.completion.chunk",
                    "created": 0,
                    "model": MODELO,
                    "choices": [],
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 5,
                        "total_tokens": 15,
                        "prompt_tokens_details": {"cached_tokens": 0},
                    },
                }
            )
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            # El cliente cerró el flujo (turno cancelado): no es un fallo del guion.
            pass

    def enviar_evento(self, evento):
        datos = json.dumps(evento, ensure_ascii=False)
        self.wfile.write(f"data: {datos}\n\n".encode("utf-8"))
        self.wfile.flush()


def main():
    analizador = argparse.ArgumentParser(description="Proveedor OpenAI falso del arnés de humo.")
    analizador.add_argument("--port", type=int, required=True)
    analizador.add_argument("--log", required=True)
    argumentos = analizador.parse_args()

    global ARCHIVO_LOG
    ARCHIVO_LOG = os.path.abspath(argumentos.log)
    directorio = os.path.dirname(ARCHIVO_LOG)
    if directorio:
        os.makedirs(directorio, exist_ok=True)

    servidor = ThreadingHTTPServer(("127.0.0.1", argumentos.port), Manejador)
    servidor.daemon_threads = True
    print(f"PROVEEDOR_LISTO {argumentos.port}", flush=True)
    try:
        servidor.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        servidor.server_close()


if __name__ == "__main__":
    main()
