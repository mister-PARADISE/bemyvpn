"""Разбор строки с прокси в любом привычном виде и проверка: жив ли, как быстро, куда выходит."""

from __future__ import annotations

import ipaddress
import re
import time
from dataclasses import dataclass
from typing import Any

import httpx

SCHEMES = ("http", "https", "socks5", "socks5h")
_HOST = re.compile(r"^[A-Za-z0-9](?:[A-Za-z0-9.-]{0,251}[A-Za-z0-9])?$")


class ProxyParseError(ValueError):
    pass


@dataclass(frozen=True)
class ParsedProxy:
    scheme: str
    host: str
    port: int
    username: str = ""
    password: str = ""


def _hostport(text: str) -> tuple[str, int] | None:
    text = text.strip()
    m = re.match(r"^\[([0-9a-fA-F:.]+)\]:(\d+)$", text)  # [::1]:8080
    if m:
        return m.group(1), int(m.group(2))
    host, sep, port = text.rpartition(":")
    if not sep or not port.isdigit() or not host:
        return None
    return host, int(port)


def parse_proxy(line: str, default_scheme: str = "http") -> ParsedProxy:
    """host:port | user:pass@host:port | host:port@user:pass | host:port:user:pass | scheme://…"""
    s = line.strip()
    if not s:
        raise ProxyParseError("пустая строка")
    scheme = default_scheme
    if "://" in s:
        scheme, s = s.split("://", 1)
        scheme = scheme.lower()
    if scheme == "socks":
        scheme = "socks5"
    if scheme not in SCHEMES:
        raise ProxyParseError(f"тип «{scheme}» не поддерживается — нужен http, https, socks5 или socks5h")
    s = s.rstrip("/")
    user = pwd = ""
    hp = None
    if "@" in s:
        left, right = s.rsplit("@", 1)
        if (hp := _hostport(right)) is not None:
            user, _, pwd = left.partition(":")
        elif (hp := _hostport(left)) is not None:
            user, _, pwd = right.partition(":")
    else:
        parts = s.split(":")
        if len(parts) == 4 and parts[1].isdigit():  # host:port:user:pass
            hp, user, pwd = (parts[0], int(parts[1])), parts[2], parts[3]
        else:
            hp = _hostport(s)
    if hp is None:
        raise ProxyParseError("не нашёл адрес и порт")
    host, port = hp
    if not 1 <= port <= 65535:
        raise ProxyParseError(f"порт {port} вне 1–65535")
    try:
        host = str(ipaddress.ip_address(host))
    except ValueError:
        if not _HOST.match(host):
            raise ProxyParseError(f"странный адрес «{host}»") from None
        host = host.lower()
    return ParsedProxy(scheme, host, port, user, pwd)


def _first(data: dict[str, Any], *keys: str) -> str:
    for key in keys:
        value = data
        for part in key.split("."):
            value = value.get(part) if isinstance(value, dict) else None
        if value not in (None, ""):
            return str(value)
    return ""


def read_exit(resp: httpx.Response) -> dict[str, str]:
    """Понимает ответы ipinfo.io, ip-api.com, ipify и просто текст с IP."""
    try:
        data = resp.json()
    except ValueError:
        text = resp.text.strip()
        try:
            return {"exit_ip": str(ipaddress.ip_address(text))}
        except ValueError:
            return {}
    if not isinstance(data, dict):
        return {}
    return {
        "exit_ip": _first(data, "ip", "query", "ip_addr", "origin"),
        "country": _first(data, "country", "countryCode", "country_code"),
        "city": _first(data, "city"),
        "org": _first(data, "org", "isp", "asn.name", "connection.isp"),
    }


async def check(url: str, proxy_url: str, timeout: float) -> dict[str, Any]:
    start = time.perf_counter()
    try:
        async with httpx.AsyncClient(proxy=proxy_url, timeout=timeout, follow_redirects=True, trust_env=False,
                                     headers={"User-Agent": "Perimeter proxy check"}) as client:
            resp = await client.get(url)
        ms = round((time.perf_counter() - start) * 1000, 1)
        if resp.status_code == 407:
            return {"status": "dead", "error": "прокси не принял логин или пароль (407)", "latency_ms": ms}
        if resp.status_code >= 400:
            return {"status": "dead", "error": f"проверочный сайт ответил {resp.status_code}", "latency_ms": ms}
        return {"status": "alive", "error": "", "latency_ms": ms, **read_exit(resp)}
    except httpx.ProxyError as exc:
        return {"status": "dead", "error": f"прокси отказал: {exc}"[:200]}
    except httpx.TimeoutException:
        return {"status": "dead", "error": f"нет ответа за {timeout:.0f} с"}
    except httpx.ConnectError as exc:
        return {"status": "dead", "error": f"не подключиться: {exc}"[:200]}
    except (httpx.HTTPError, httpx.InvalidURL, OSError, ValueError) as exc:
        return {"status": "dead", "error": f"{type(exc).__name__}: {exc}"[:200]}
    except Exception as exc:  # noqa: BLE001 — что бы ни случилось, у прокси должен появиться итог
        return {"status": "dead", "error": f"проверка не удалась: {type(exc).__name__}: {exc}"[:200]}
