"""Локальная «сеть» для тестов: DNS-сервер, TCP-сервисы, HTTP/TLS-сайт, HTTP-прокси, эхо адреса.

Модули разведки проверяются по-настоящему — сокеты, DNS, TLS, — но без интернета.
"""

from __future__ import annotations

import base64
import json
import socket
import socketserver
import ssl
import subprocess
import threading
import time
from pathlib import Path
from typing import Callable

import dns.message
import dns.rcode
import dns.rdatatype
import dns.rrset


class FakeDNS:
    """UDP DNS-сервер по словарю {(имя, тип): [значения]}. CNAME разворачивает как настоящий."""

    def __init__(self, zone: dict[tuple[str, str], list[str]]) -> None:
        self.zone = {(n.lower().rstrip("."), t.upper()): v for (n, t), v in zone.items()}
        self.names = {n for n, _ in self.zone}
        self.queries: list[tuple[str, str]] = []

    def __enter__(self) -> "FakeDNS":
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.settimeout(0.2)
        self.port = self.sock.getsockname()[1]
        self.running = True
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *exc) -> None:
        self.running = False
        self.thread.join(2)
        self.sock.close()

    def _serve(self) -> None:
        while self.running:
            try:
                data, addr = self.sock.recvfrom(4096)
            except (socket.timeout, OSError):
                continue
            try:
                self.sock.sendto(self._answer(dns.message.from_wire(data)).to_wire(), addr)
            except Exception:  # noqa: BLE001 — тестовый сервер не должен падать
                pass

    def _answer(self, query: dns.message.Message) -> dns.message.Message:
        resp = dns.message.make_response(query)
        q = query.question[0]
        name, rtype = q.name.to_text().rstrip(".").lower(), dns.rdatatype.to_text(q.rdtype)
        self.queries.append((name, rtype))
        current = name
        for _ in range(5):
            if (current, rtype) in self.zone:
                resp.answer.append(dns.rrset.from_text_list(current + ".", 60, "IN", rtype, self.zone[(current, rtype)]))
                return resp
            if rtype != "CNAME" and (current, "CNAME") in self.zone:
                target = self.zone[(current, "CNAME")][0]
                resp.answer.append(dns.rrset.from_text_list(current + ".", 60, "IN", "CNAME", [target]))
                current = target.rstrip(".").lower()
                continue
            break
        if not resp.answer and current not in self.names:
            resp.set_rcode(dns.rcode.NXDOMAIN)
        return resp


class _Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


class Service:
    """TCP-сервис на 127.0.0.1:случайный порт. handler(conn) делает всё сам."""

    def __init__(self, handler: Callable[[socket.socket], None], tls: ssl.SSLContext | None = None) -> None:
        outer = self

        class Handler(socketserver.BaseRequestHandler):
            def handle(self) -> None:
                conn = self.request
                try:
                    if outer.tls is not None:
                        conn = outer.tls.wrap_socket(conn, server_side=True)
                    handler(conn)
                except (OSError, ssl.SSLError):
                    pass

        self.tls = tls
        self.server = _Server(("127.0.0.1", 0), Handler)
        self.port = self.server.server_address[1]
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)

    def __enter__(self) -> "Service":
        self.thread.start()
        return self

    def __exit__(self, *exc) -> None:
        self.stop()

    def stop(self) -> None:
        self.server.shutdown()
        self.server.server_close()


def greeter(banner: bytes) -> Callable[[socket.socket], None]:
    def handle(conn: socket.socket) -> None:
        conn.sendall(banner)
        time.sleep(0.3)
    return handle


def read_request(conn: socket.socket) -> tuple[str, dict[str, str]]:
    data = b""
    conn.settimeout(3)
    while b"\r\n\r\n" not in data:
        chunk = conn.recv(4096)
        if not chunk:
            break
        data += chunk
    head = data.split(b"\r\n\r\n", 1)[0].decode("latin-1").split("\r\n")
    headers = {}
    for line in head[1:]:
        if ":" in line:
            k, v = line.split(":", 1)
            headers[k.strip().lower()] = v.strip()
    return head[0], headers


def http_app(route: Callable[[str, dict[str, str]], tuple[int, dict[str, str], bytes]]):
    def handle(conn: socket.socket) -> None:
        line, headers = read_request(conn)
        status, extra, body = route(line, headers)
        head = [f"HTTP/1.1 {status} X", f"Content-Length: {len(body)}", "Connection: close"]
        head += [f"{k}: {v}" for k, v in extra.items()]
        conn.sendall(("\r\n".join(head) + "\r\n\r\n").encode() + body)
    return handle


def http_proxy(user: str = "", password: str = "") -> Callable[[socket.socket], None]:
    """Простой HTTP-прокси для http:// адресов (absolute-form), с Basic-авторизацией по желанию."""
    def handle(conn: socket.socket) -> None:
        line, headers = read_request(conn)
        if user:
            expected = "Basic " + base64.b64encode(f"{user}:{password}".encode()).decode()
            if headers.get("proxy-authorization") != expected:
                conn.sendall(b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n"
                             b"Proxy-Authenticate: Basic realm=x\r\nConnection: close\r\n\r\n")
                return
        method, url, _ver = line.split(" ")
        rest = url.split("://", 1)[1]
        hostport, _, path = rest.partition("/")
        host, _, port = hostport.partition(":")
        upstream = socket.create_connection((host, int(port or 80)), timeout=3)
        upstream.sendall(f"{method} /{path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\n\r\n".encode())
        while chunk := upstream.recv(65536):
            conn.sendall(chunk)
        upstream.close()
    return handle


def ip_echo(answer: dict) -> Callable[[socket.socket], None]:
    body = json.dumps(answer).encode()
    return http_app(lambda line, headers: (200, {"Content-Type": "application/json"}, body))


def self_signed(dirpath: Path, names: list[str], days: int = 10) -> tuple[Path, Path] | None:
    """Сертификат для тестов (через openssl). None — если openssl нет."""
    cert, key = dirpath / "cert.pem", dirpath / "key.pem"
    san = ",".join(f"DNS:{n}" for n in names)
    try:
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", str(key), "-out", str(cert),
                        "-days", str(days), "-subj", f"/CN={names[0]}/O=Perimeter Test CA", "-addext",
                        f"subjectAltName={san}"], check=True, capture_output=True, timeout=60)
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired):
        return None
    return cert, key
