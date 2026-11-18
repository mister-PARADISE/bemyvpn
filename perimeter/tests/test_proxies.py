"""Прокси: разбор форматов, проверка через настоящий локальный прокси, страница, строка на обзоре."""

from __future__ import annotations

import asyncio
import time

import pytest
from sqlalchemy import select

from perimeter.extensions.proxies.checker import ProxyParseError, check, parse_proxy
from perimeter.extensions.proxies.models import Proxy

from .conftest import flashes
from .netlab import Service, http_proxy, ip_echo

EXIT = {"ip": "203.0.113.7", "country": "DE", "city": "Berlin", "org": "AS64500 Test Net"}


@pytest.mark.parametrize("line,expected", [
    ("1.2.3.4:8080", ("http", "1.2.3.4", 8080, "", "")),
    ("user:pa:ss@1.2.3.4:8080", ("http", "1.2.3.4", 8080, "user", "pa:ss")),
    ("1.2.3.4:8080@user:pass", ("http", "1.2.3.4", 8080, "user", "pass")),
    ("1.2.3.4:8080:user:pass", ("http", "1.2.3.4", 8080, "user", "pass")),
    ("socks5://u:p@Proxy.Example.com:1080/", ("socks5", "proxy.example.com", 1080, "u", "p")),
    ("socks://5.6.7.8:1080", ("socks5", "5.6.7.8", 1080, "", "")),
    ("[2001:db8::1]:3128", ("http", "2001:db8::1", 3128, "", "")),
])
def test_parse_formats(line, expected):
    p = parse_proxy(line)
    assert (p.scheme, p.host, p.port, p.username, p.password) == expected


@pytest.mark.parametrize("line", ["", "1.2.3.4", "1.2.3.4:99999", "ftp://1.2.3.4:21", "bad host:80"])
def test_parse_rejects(line):
    with pytest.raises(ProxyParseError):
        parse_proxy(line)


def test_check_alive_with_auth_dead_and_wrong_password():
    with Service(ip_echo(EXIT)) as echo, Service(http_proxy("u", "secret")) as proxy:
        url = f"http://127.0.0.1:{echo.port}/json"
        ok = asyncio.run(check(url, f"http://u:secret@127.0.0.1:{proxy.port}", 5))
        assert ok["status"] == "alive" and ok["exit_ip"] == "203.0.113.7" and ok["country"] == "DE"
        assert ok["org"] == "AS64500 Test Net" and ok["latency_ms"] > 0
        wrong = asyncio.run(check(url, f"http://u:nope@127.0.0.1:{proxy.port}", 5))
        assert wrong["status"] == "dead" and "407" in wrong["error"]
    dead = asyncio.run(check("http://127.0.0.1:9/", f"http://127.0.0.1:{proxy.port}", 2))
    assert dead["status"] == "dead" and dead["error"]


def test_proxies_page_add_check_and_stat(browser, ctx):
    with Service(ip_echo(EXIT)) as echo, Service(http_proxy()) as proxy:
        ctx.settings.set("proxies.check_url", f"http://127.0.0.1:{echo.port}/json")
        ctx.settings.set("proxies.timeout", 3)
        lines = f"127.0.0.1:{proxy.port}\n127.0.0.1:1\nне прокси\n127.0.0.1:{proxy.port}\n# комментарий"
        r = browser.post("/proxies", {"lines": lines, "scheme": "http", "label": "тест", "check": "1"})
        assert r.status_code == 303
        note = flashes(browser.get("/proxies").text)
        assert "добавлено 2" in note and "уже были 1" in note and "не разобрал 1" in note
        deadline = time.monotonic() + 15
        while True:
            with ctx.db.session() as s:
                states = {p.port: p for p in s.scalars(select(Proxy))}
            if all(p.status in ("alive", "dead") for p in states.values()):
                break
            assert time.monotonic() < deadline, {k: v.status for k, v in states.items()}
            time.sleep(0.1)
        good, bad = states[proxy.port], states[1]
        assert good.status == "alive" and good.exit_ip == "203.0.113.7" and good.label == "тест"
        assert bad.status == "dead"
        assert "1 живой из 2" in browser.get("/").text  # строка «Панели» на обзоре
        page = browser.get("/proxies?state=alive").text
        assert "203.0.113.7" in page and "127.0.0.1:1<" not in page
        browser.post("/proxies/delete-dead")
        with ctx.db.session() as s:
            assert [p.port for p in s.scalars(select(Proxy))] == [proxy.port]
        assert ctx.services["proxies"].pick() == f"http://127.0.0.1:{proxy.port}"
