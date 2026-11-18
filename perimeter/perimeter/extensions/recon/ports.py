"""Списки портов, названия сервисов и снятие «приветствия» сервиса."""

from __future__ import annotations

import asyncio
import ssl
from contextlib import suppress

TOP = (
    7, 9, 13, 21, 22, 23, 25, 26, 37, 53, 79, 80, 81, 88, 106, 110, 111, 113, 119, 135, 139, 143, 144, 179, 199,
    389, 427, 443, 444, 445, 465, 513, 514, 515, 543, 544, 548, 554, 587, 631, 646, 873, 990, 993, 995, 1025, 1026,
    1027, 1028, 1029, 1110, 1433, 1720, 1723, 1755, 1900, 2000, 2001, 2049, 2121, 2717, 3000, 3128, 3306, 3389,
    3986, 4899, 5000, 5009, 5051, 5060, 5101, 5190, 5357, 5432, 5631, 5666, 5800, 5900, 6000, 6001, 6646, 7070,
    8000, 8008, 8009, 8080, 8081, 8443, 8888, 9100, 9999, 10000, 32768, 49152, 49153, 49154, 49155, 49156, 49157,
)
# Что не должно смотреть в интернет: базы, кэши, панели, удалённый доступ, служебные API.
RISKY = {
    21: "ftp", 23: "telnet", 445: "smb", 139: "netbios", 1433: "mssql", 1521: "oracle", 2049: "nfs",
    2375: "docker", 2376: "docker-tls", 3306: "mysql", 3389: "rdp", 5432: "postgresql", 5601: "kibana",
    5900: "vnc", 5984: "couchdb", 6379: "redis", 6443: "kubernetes", 8086: "influxdb", 9000: "php-fpm/portainer",
    9200: "elasticsearch", 10250: "kubelet", 11211: "memcached", 15672: "rabbitmq", 27017: "mongodb", 873: "rsync",
}
WEB = (80, 443, 3000, 5000, 8000, 8008, 8080, 8081, 8443, 8888, 9000, 9443)

SERVICES = {
    7: "echo", 21: "ftp", 22: "ssh", 23: "telnet", 25: "smtp", 53: "dns", 80: "http", 81: "http", 110: "pop3",
    111: "rpcbind", 135: "msrpc", 139: "netbios", 143: "imap", 389: "ldap", 443: "https", 445: "smb", 465: "smtps",
    587: "submission", 631: "ipp", 873: "rsync", 993: "imaps", 995: "pop3s", 1433: "mssql", 1521: "oracle",
    1723: "pptp", 2049: "nfs", 2375: "docker", 2376: "docker-tls", 3000: "http", 3128: "squid", 3306: "mysql",
    3389: "rdp", 5000: "http", 5060: "sip", 5432: "postgresql", 5601: "kibana", 5672: "amqp", 5900: "vnc",
    5984: "couchdb", 6379: "redis", 6443: "kubernetes", 8000: "http", 8008: "http", 8080: "http-alt",
    8081: "http", 8086: "influxdb", 8443: "https-alt", 8888: "http", 9000: "http", 9100: "jetdirect",
    9200: "elasticsearch", 9443: "https", 10000: "webmin", 10250: "kubelet", 11211: "memcached",
    15672: "rabbitmq", 27017: "mongodb",
}
HTTP_PORTS = {80, 81, 3000, 5000, 5601, 8000, 8008, 8080, 8081, 8086, 8888, 9000, 9200, 10000, 15672, 2375}
TLS_PORTS = {443, 444, 4443, 8443, 9443, 6443, 10250, 2376}
GREETS_FIRST = {21, 22, 25, 110, 143, 587, 3306, 5900, 1723, 2121}

PRESETS = {
    "top": "Частые 100",
    "risky": "Базы и панели — что не должно торчать наружу",
    "web": "Веб: 80, 443, 3000, 5000, 8000–8888, 9000, 9443",
    "custom": "Свои",
}


def parse_ports(text: str, limit: int = 10000) -> list[int]:
    """«22, 80, 8000-8100» → [22, 80, 8000, …]. ValueError с понятным текстом при ошибке."""
    out: set[int] = set()
    for chunk in text.replace(";", ",").replace(" ", ",").split(","):
        chunk = chunk.strip()
        if not chunk:
            continue
        lo, sep, hi = chunk.partition("-")
        if not lo.isdigit() or (sep and not hi.isdigit()):
            raise ValueError(f"не понял «{chunk}»")
        a, b = int(lo), int(hi) if sep else int(lo)
        if not (1 <= a <= 65535 and 1 <= b <= 65535) or b < a:
            raise ValueError(f"порт вне 1–65535 или диапазон наоборот: «{chunk}»")
        out.update(range(a, b + 1))
        if len(out) > limit:
            raise ValueError(f"больше {limit} портов за раз")
    return sorted(out)


def ports_for(preset: str, custom: str) -> list[int]:
    if preset == "risky":
        return sorted(RISKY)
    if preset == "web":
        return list(WEB)
    if preset == "custom":
        return parse_ports(custom or "")
    return list(TOP)


def _clean(data: bytes) -> str:
    text = data.decode("utf-8", "replace")
    line = next((ln for ln in text.splitlines() if ln.strip()), "")
    return "".join(ch for ch in line if ch.isprintable())[:120]


def _mysql(data: bytes) -> str:
    # пакет: 3 байта длины, 1 номер, протокол (10), версия до \0
    if len(data) > 6 and data[4] == 10:
        return "MySQL " + data[5:].split(b"\0", 1)[0].decode("latin-1", "replace")[:60]
    return _clean(data)


async def banner(run, ip: str, port: int, host: str, timeout: float) -> tuple[str, bool]:
    """(что ответил сервис, работает ли через TLS). Короткий вежливый запрос, не атака."""
    tls = port in TLS_PORTS
    try:
        if tls:
            ctx = ssl.create_default_context()
            ctx.check_hostname, ctx.verify_mode = False, ssl.CERT_NONE
            reader, writer = await run.connect(ip, port, timeout, ssl=ctx, server_hostname=host or None)
        else:
            reader, writer = await run.connect(ip, port, timeout)
    except (OSError, TimeoutError, ssl.SSLError):
        return "", False
    try:
        if tls or port in HTTP_PORTS:
            writer.write(f"HEAD / HTTP/1.0\r\nHost: {host or ip}\r\nUser-Agent: Perimeter\r\n\r\n".encode())
            await writer.drain()
            data = await asyncio.wait_for(reader.read(2048), timeout + 1)
            text = data.decode("latin-1", "replace")
            status = text.split("\r\n", 1)[0]
            server = next((ln.split(":", 1)[1].strip() for ln in text.split("\r\n")
                           if ln.lower().startswith("server:")), "")
            return (f"{status}  {server}".strip() if status.startswith("HTTP/") else _clean(data)), tls
        data = await asyncio.wait_for(reader.read(1024), 1.5 if port in GREETS_FIRST else 1.0)
        return (_mysql(data) if port == 3306 else _clean(data)), tls
    except (OSError, TimeoutError, ssl.SSLError, asyncio.IncompleteReadError):
        return "", tls
    finally:
        writer.close()
        with suppress(Exception):
            await asyncio.wait_for(writer.wait_closed(), 1)
