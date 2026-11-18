"""Проба веба: один HTTP-запрос на конкретный IP с нужным именем (Host и SNI).

Свой маленький клиент, а не httpx: так запрос гарантированно уходит на адрес,
который движок уже проверил на принадлежность цели, и видно сертификат даже
когда он невалиден.
"""

from __future__ import annotations

import asyncio
import re
import ssl
import time
from contextlib import suppress
from datetime import datetime, timezone
from typing import Any
from urllib.parse import urljoin, urlsplit

from ... import __version__

UA = f"Mozilla/5.0 (compatible; Perimeter/{__version__})"
# Как проверять сертификат: системные корневые. Тесты подставляют свой удостоверяющий центр.
verify_context = ssl.create_default_context
TITLE = re.compile(rb"<title[^>]*>(.*?)</title>", re.I | re.S)


def _name(pairs) -> str:
    flat = {k: v for rdn in pairs or () for k, v in rdn}
    return flat.get("organizationName") or flat.get("commonName") or ""


def cert_summary(cert: dict[str, Any]) -> dict[str, Any]:
    if not cert:
        return {}
    out: dict[str, Any] = {"issuer": _name(cert.get("issuer")), "subject": _name(cert.get("subject")),
                           "sans": [v for k, v in cert.get("subjectAltName", ()) if k == "DNS"]}
    if cert.get("notAfter"):
        expires = datetime.fromtimestamp(ssl.cert_time_to_seconds(cert["notAfter"]), timezone.utc)
        out["expires"] = expires.strftime("%Y-%m-%d")
        out["days_left"] = (expires - datetime.now(timezone.utc)).days
    return out


def _unverified() -> ssl.SSLContext:
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx


# Почему сертификат не прошёл проверку — словами, понятными владельцу сайта
VERIFY_ERRORS = (
    ("certificate has expired", "сертификат истёк"),
    ("not yet valid", "сертификат ещё не начал действовать"),
    ("self-signed certificate in certificate chain", "в цепочке самоподписанный сертификат"),
    ("self signed certificate in certificate chain", "в цепочке самоподписанный сертификат"),
    ("self-signed certificate", "самоподписанный: браузеры ему не верят"),
    ("self signed certificate", "самоподписанный: браузеры ему не верят"),
    ("unable to get local issuer certificate", "неполная цепочка: сервер не отдаёт промежуточный сертификат"),
    ("hostname mismatch", "выдан на другое имя"),
    ("ip address mismatch", "выдан на другой адрес"),
    ("certificate has been revoked", "сертификат отозван"),
)


def explain_verify(message: str) -> str:
    low = (message or "").lower()
    for needle, text in VERIFY_ERRORS:
        if needle in low:
            return text
    return message or "не прошёл проверку"


def _tls_details(writer) -> dict[str, Any]:
    obj = writer.get_extra_info("ssl_object")
    if obj is None:
        return {}
    out: dict[str, Any] = {"protocol": obj.version() or ""}
    if cipher := obj.cipher():
        out["cipher"], out["bits"] = cipher[0], cipher[2]
    return out


async def tls_probe(run, ip: str, host: str, port: int = 443, timeout: float = 8.0,
                    verify_ctx: ssl.SSLContext | None = None) -> dict[str, Any]:
    """Только рукопожатие TLS с нужным именем (SNI): сертификат, его проверка, версия протокола, шифр.
    Невалидный сертификат — не ошибка проверки: пишем почему, а версию и шифр узнаём без проверки."""
    try:
        try:
            _r, writer = await run.connect(ip, port, timeout, ssl=verify_ctx or verify_context(), server_hostname=host)
            out: dict[str, Any] = {"valid": True, **cert_summary(writer.get_extra_info("peercert") or {})}
        except ssl.SSLCertVerificationError as exc:
            raw = exc.verify_message or str(exc)
            out = {"valid": False, "error": explain_verify(raw), "raw": raw}
            _r, writer = await run.connect(ip, port, timeout, ssl=_unverified(), server_hostname=host)
    except (OSError, TimeoutError, ssl.SSLError) as exc:
        return {"failed": _why(exc)}
    out.update(_tls_details(writer))
    writer.close()
    with suppress(Exception):
        await asyncio.wait_for(writer.wait_closed(), 1)
    return out


# Заголовки, которыми сайт защищает посетителя. Нет — не катастрофа, но каждый закрывает свой класс атак.
SECURITY_HEADERS = (
    ("content-security-policy", "CSP"),
    ("x-frame-options", "X-Frame-Options"),
    ("x-content-type-options", "X-Content-Type-Options"),
    ("referrer-policy", "Referrer-Policy"),
)


def security_report(headers: dict[str, str]) -> list[str]:
    """Каких защитных заголовков нет. X-Frame-Options не нужен, если в CSP есть frame-ancestors."""
    missing = [label for key, label in SECURITY_HEADERS if key not in headers]
    if "X-Frame-Options" in missing and "frame-ancestors" in headers.get("content-security-policy", "").lower():
        missing.remove("X-Frame-Options")
    return missing


def parse_cookie(line: str) -> dict[str, Any]:
    name = line.split("=", 1)[0].strip()
    attrs = {p.strip().split("=", 1)[0].lower(): (p.split("=", 1)[1].strip() if "=" in p else "")
             for p in line.split(";")[1:]}
    return {"name": name[:64], "secure": "secure" in attrs, "httponly": "httponly" in attrs,
            "samesite": attrs.get("samesite", "")}


async def fetch(run, ip: str, host: str, port: int, tls: bool, path: str = "/", timeout: float = 8.0,
                verify_ctx: ssl.SSLContext | None = None, keep: bool = False, max_body: int = 262144,
                extra_headers: dict[str, str] | None = None) -> dict[str, Any]:
    """GET на ip:port с Host: host. Возвращает статус, заголовки, title, TLS, время — или error.
    keep=True — ещё все заголовки (headers) и текст ответа (body, до max_body байт)."""
    scheme = "https" if tls else "http"
    default = 443 if tls else 80
    url = f"{scheme}://{host}{'' if port == default else f':{port}'}{path}"
    out: dict[str, Any] = {"url": url, "ip": ip}
    start = time.perf_counter()
    try:
        if tls:
            try:
                reader, writer = await run.connect(ip, port, timeout, ssl=verify_ctx or verify_context(),
                                                   server_hostname=host)
                out["tls"] = {"valid": True, **cert_summary(writer.get_extra_info("peercert") or {})}
            except ssl.SSLCertVerificationError as exc:
                out["tls"] = {"valid": False, "error": exc.verify_message or str(exc)}
                reader, writer = await run.connect(ip, port, timeout, ssl=_unverified(), server_hostname=host)
        else:
            reader, writer = await run.connect(ip, port, timeout)
    except (OSError, TimeoutError, ssl.SSLError) as exc:
        out["error"] = _why(exc)
        return out
    try:
        extra = "".join(f"{k}: {v}\r\n" for k, v in (extra_headers or {}).items())
        writer.write((f"GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {UA}\r\n"
                      f"Accept: text/html,*/*;q=0.8\r\nAccept-Encoding: identity\r\n{extra}Connection: close\r\n\r\n").encode())
        await writer.drain()
        head = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), timeout)
        out["ms"] = round((time.perf_counter() - start) * 1000, 1)
        body = b""
        limit = max_body if keep else 65536
        with suppress(TimeoutError, OSError, asyncio.IncompleteReadError):
            deadline = time.monotonic() + 4
            while len(body) < limit and time.monotonic() < deadline:  # тело целиком, а не первый кусок
                chunk = await asyncio.wait_for(reader.read(min(65536, limit - len(body))), max(0.1, deadline - time.monotonic()))
                if not chunk:
                    break
                body += chunk
    except (OSError, TimeoutError, asyncio.LimitOverrunError, asyncio.IncompleteReadError, ssl.SSLError) as exc:
        out["error"] = _why(exc)
        return out
    finally:
        writer.close()
        with suppress(Exception):
            await asyncio.wait_for(writer.wait_closed(), 1)

    lines = head.decode("latin-1").split("\r\n")
    status = lines[0].split(" ", 2)
    out["status"] = int(status[1]) if len(status) > 1 and status[1].isdigit() else 0
    headers: dict[str, str] = {}
    cookies: list[dict[str, Any]] = []
    for line in lines[1:]:
        if ":" in line:
            k, v = line.split(":", 1)
            key = k.strip().lower()
            if key == "set-cookie" and len(cookies) < 20:
                cookies.append(parse_cookie(v.strip()))
            headers.setdefault(key, v.strip())
    out["server"] = headers.get("server", "")
    out["powered_by"] = headers.get("x-powered-by", "")
    out["location"] = headers.get("location", "")
    out["hsts"] = "strict-transport-security" in headers
    out["missing"] = security_report(headers)
    out["cookies"] = cookies
    if m := TITLE.search(body):
        charset = re.search(r"charset=([\w-]+)", headers.get("content-type", ""))
        try:
            text = m.group(1).decode(charset.group(1) if charset else "utf-8", "replace")
        except LookupError:
            text = m.group(1).decode("utf-8", "replace")
        out["title"] = " ".join(text.split())[:120]
    if keep:
        out["headers"] = headers
        charset = re.search(r"charset=([\w-]+)", headers.get("content-type", ""))
        try:
            out["body"] = body.decode(charset.group(1) if charset else "utf-8", "replace")
        except LookupError:
            out["body"] = body.decode("utf-8", "replace")
    return out


def _why(exc: BaseException) -> str:
    if isinstance(exc, TimeoutError):
        return "нет ответа (таймаут)"
    if isinstance(exc, ConnectionRefusedError):
        return "соединение отклонено"
    return f"{type(exc).__name__}: {exc}"[:200]


def next_hop(current_url: str, location: str) -> tuple[str, str, int, bool, str] | None:
    """Location → (url, host, port, tls, path) или None."""
    if not location:
        return None
    url = urljoin(current_url, location)
    parts = urlsplit(url)
    if parts.scheme not in ("http", "https") or not parts.hostname:
        return None
    tls = parts.scheme == "https"
    path = parts.path or "/"
    if parts.query:
        path += "?" + parts.query
    return url, parts.hostname.lower(), parts.port or (443 if tls else 80), tls, path
