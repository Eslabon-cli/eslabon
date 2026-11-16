#!/usr/bin/env python3
"""Centinela de red para el arnés de humo de Eslabon.

Proxy HTTP/HTTPS (método CONNECT) que escucha en 127.0.0.1.  Las conexiones a
127.0.0.1/localhost se dejan pasar; TODO lo demás se rechaza con 403 y se anota
en el log como una línea JSON con host, puerto y momento.  No reenvía tráfico
externo: es una barrera con bitácora.

    python3 centinela.py --port 8098 --log centinela.jsonl

Solo biblioteca estándar, Python 3.9+.
"""

import argparse
import http.client
import json
import os
import select
import socket
import threading
from datetime import datetime
from http.server import BaseHTTPRequestHandler
from http.server import ThreadingHTTPServer
from urllib.parse import urlsplit

DESTINOS_LOCALES = ("127.0.0.1", "localhost", "::1")
CERROJO_LOG = threading.Lock()
ARCHIVO_LOG = None


def registrar_intento(metodo, host, puerto):
    registro = {
        "momento": datetime.now().isoformat(timespec="seconds"),
        "metodo": metodo,
        "host": host,
        "puerto": puerto,
    }
    with CERROJO_LOG:
        with open(ARCHIVO_LOG, "a", encoding="utf-8") as archivo:
            archivo.write(json.dumps(registro, ensure_ascii=False, sort_keys=True) + "\n")
            archivo.flush()


def partir_host_puerto(texto, puerto_por_defecto):
    """Parte 'host:puerto' (sin romper IPv6 entre corchetes) en (host, puerto)."""
    if texto.startswith("["):
        cierre = texto.find("]")
        if cierre > 0:
            host = texto[1:cierre]
            resto = texto[cierre + 1 :].lstrip(":")
            return host, int(resto) if resto.isdigit() else puerto_por_defecto
    host = texto
    puerto = puerto_por_defecto
    if texto.count(":") == 1:
        host, _, candidato = texto.partition(":")
        if candidato.isdigit():
            puerto = int(candidato)
    return host, puerto


def destino_de_uri(ruta, cabeceras):
    """Extrae (host, puerto) de una URI absoluta o del impuesto Host."""
    partes = urlsplit(ruta)
    if partes.hostname:
        por_defecto = 443 if partes.scheme == "https" else 80
        return partes.hostname, partes.port or por_defecto
    impuesto = cabeceras.get("Host") or ""
    return partir_host_puerto(impuesto, 80)


def es_local(host):
    return host.lower() in DESTINOS_LOCALES


class Proxy(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "humo-centinela/1"

    def log_message(self, formato, *args):  # silencia el log de stderr
        pass

    def do_CONNECT(self):
        host, puerto = partir_host_puerto(self.path or "", 443)
        if es_local(host):
            self.tunel_local(host, puerto)
            return
        registrar_intento("CONNECT", host, puerto)
        self.rechazar(host, puerto)

    def do_GET(self):
        self.rechazar_peticion()

    def do_HEAD(self):
        self.rechazar_peticion()

    def do_POST(self):
        self.rechazar_peticion()

    def do_PUT(self):
        self.rechazar_peticion()

    def do_DELETE(self):
        self.rechazar_peticion()

    def do_OPTIONS(self):
        self.rechazar_peticion()

    def rechazar_peticion(self):
        host, puerto = destino_de_uri(self.path, self.headers)
        if es_local(host):
            self.reenviar_local(host, puerto)
            return
        registrar_intento(self.command, host, puerto)
        self.rechazar(host, puerto)

    def rechazar(self, host, puerto, motivo="host fuera de localhost"):
        cuerpo = f"HUMO_CENTINELA: {host}:{puerto} rechazado ({motivo})\n".encode("utf-8")
        self.send_response(403)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(cuerpo)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True
        try:
            self.wfile.write(cuerpo)
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass

    def tunel_local(self, host, puerto):
        """Deja pasar un CONNECT solo hacia 127.0.0.1/localhost."""
        try:
            remoto = socket.create_connection((host, puerto), timeout=5)
        except OSError as error:
            self.send_error(502, f"no se pudo conectar a {host}:{puerto}: {error}")
            return
        self.send_response(200, "Connection Established")
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True
        try:
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            remoto.close()
            return
        cliente = self.connection
        try:
            abiertos = [cliente, remoto]
            while True:
                listos, _, _ = select.select(abiertos, [], [], 5)
                if not listos:
                    continue
                for origen in listos:
                    try:
                        datos = origen.recv(65536)
                    except OSError:
                        return
                    if not datos:
                        return
                    destino = remoto if origen is cliente else cliente
                    try:
                        destino.sendall(datos)
                    except OSError:
                        return
        finally:
            remoto.close()

    def reenviar_local(self, host, puerto):
        """Reenvía una petición HTTP a un destino local (no se registra)."""
        partes = urlsplit(self.path)
        ruta = partes.path or "/"
        if partes.query:
            ruta = ruta + "?" + partes.query
        cabeceras = {}
        for nombre, valor in self.headers.items():
            if nombre.lower() in ("proxy-connection", "proxy-authorization", "connection"):
                continue
            cabeceras[nombre] = valor
        cuerpo = None
        largo = self.headers.get("Content-Length")
        if largo and largo.isdigit():
            cuerpo = self.rfile.read(int(largo))
        try:
            conexion = http.client.HTTPConnection(host, puerto, timeout=10)
            conexion.request(self.command, ruta, body=cuerpo, headers=cabeceras)
            respuesta = conexion.getresponse()
            self.send_response(respuesta.status, respuesta.reason)
            for nombre, valor in respuesta.getheaders():
                if nombre.lower() in ("transfer-encoding", "content-length", "connection"):
                    continue
                self.send_header(nombre, valor)
            self.send_header("Connection", "close")
            self.end_headers()
            self.close_connection = True
            while True:
                datos = respuesta.read(65536)
                if not datos:
                    break
                self.wfile.write(datos)
                self.wfile.flush()
            conexion.close()
        except (OSError, http.client.HTTPException) as error:
            self.send_error(502, f"no se pudo reenviar a {host}:{puerto}: {error}")


def main():
    analizador = argparse.ArgumentParser(description="Centinela de red del arnés de humo.")
    analizador.add_argument("--port", type=int, required=True)
    analizador.add_argument("--log", required=True)
    argumentos = analizador.parse_args()

    global ARCHIVO_LOG
    ARCHIVO_LOG = os.path.abspath(argumentos.log)
    directorio = os.path.dirname(ARCHIVO_LOG)
    if directorio:
        os.makedirs(directorio, exist_ok=True)
    with open(ARCHIVO_LOG, "a", encoding="utf-8"):
        pass

    servidor = ThreadingHTTPServer(("127.0.0.1", argumentos.port), Proxy)
    servidor.daemon_threads = True
    print(f"CENTINELA_LISTO {argumentos.port}", flush=True)
    try:
        servidor.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        servidor.server_close()


if __name__ == "__main__":
    main()
