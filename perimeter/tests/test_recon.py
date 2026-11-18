"""Модули разведки на настоящих сокетах и DNS, но без интернета (tests/netlab.py)."""

from __future__ import annotations

import ssl

import pytest
from sqlalchemy import select

from perimeter.extensions.recon import modules as recon, netkit, ports as portlib, web
from perimeter.extensions.runs.models import Asset, Run
from perimeter.extensions.targets.models import Target

from .conftest import run_and_wait
from .netlab import FakeDNS, Service, greeter, http_app, self_signed

CF_TXT = '"13335 | 104.16.0.0/13 | US | arin | 2014-03-28"'
CF_NAME = '"13335 | US | arin | 2010-07-14 | CLOUDFLARENET - Cloudflare, Inc., US"'

ZONE = {
    ("shop.test", "A"): ["127.0.0.1"],
    ("www.shop.test", "CNAME"): ["shop.test."],
    ("shop.test", "NS"): ["ns1.yandex.net.", "ns2.yandex.net."],
    ("shop.test", "MX"): ["10 mx.yandex.net."],
    ("1.0.0.127.in-addr.arpa", "PTR"): ["localhost."],
    # сайт за Cloudflare
    ("cdn.test", "A"): ["104.16.1.1"],
    ("1.1.16.104.origin.asn.cymru.com", "TXT"): [CF_TXT],
    ("AS13335.asn.cymru.com", "TXT"): [CF_NAME],
    # сайт у хостера, с соседями
    ("net.test", "A"): ["198.51.100.10"],
    ("10.100.51.198.origin.asn.cymru.com", "TXT"): ['"64500 | 198.51.100.0/24 | NL | ripencc | 2010-01-01"'],
    ("AS64500.asn.cymru.com", "TXT"): ['"64500 | NL | ripencc | 2010-01-01 | EXAMPLE-NET - Example Hosting B.V., NL"'],
    ("10.100.51.198.in-addr.arpa", "PTR"): ["web10.isp.example."],
    ("11.100.51.198.in-addr.arpa", "PTR"): ["mail.net.test."],
    ("12.100.51.198.in-addr.arpa", "PTR"): ["host12.isp.example."],
}


@pytest.fixture
def dnsd():
    with FakeDNS(ZONE) as server:
        yield server


def add_target(browser, ctx, dnsd, domain: str, **extra) -> int:
    ctx.settings.set("recon.dns_servers", [f"127.0.0.1:{dnsd.port}"])
    ctx.settings.set("recon.dns_timeout", 2)
    r = browser.post("/targets/new", {"domain": domain, **extra})
    assert r.status_code == 303, r.text
    return int(r.headers["location"].rsplit("/", 1)[1])


def assets(ctx, tid, kind):
    with ctx.db.session() as s:
        return {a.value: dict(a.data) for a in s.scalars(select(Asset).where(Asset.target_id == tid, Asset.kind == kind))}


# --- хост и провайдер --------------------------------------------------------------

def test_hostinfo_finds_ips_and_providers(browser, ctx, dnsd):
    tid = add_target(browser, ctx, dnsd, "shop.test")
    state = run_and_wait(browser, tid, "hostinfo")
    assert state["status"] == "done", state["text"]
    ip = assets(ctx, tid, "ip")["127.0.0.1"]
    assert set(ip["domains"]) == {"shop.test", "www.shop.test"} and ip["private"] and ip["ptr"] == "localhost"
    records = assets(ctx, tid, "dns_record")
    assert records["NS ns1.yandex.net"]["provider"] == "Яндекс"
    assert records["MX 10 mx.yandex.net"]["provider"] == "Яндекс 360"
    assert "почта Яндекс 360" in state["summary"] and "CNAME  shop.test" in state["text"]
    page = browser.get(f"/targets/{tid}").text
    assert "Яндекс 360" in page and "127.0.0.1" in page  # сводка и находки


def test_hostinfo_detects_cdn_and_ports_skip_it(browser, ctx, dnsd):
    tid = add_target(browser, ctx, dnsd, "cdn.test")
    run_and_wait(browser, tid, "hostinfo")
    ip = assets(ctx, tid, "ip")["104.16.1.1"]
    assert ip["cdn"] == "Cloudflare" and ip["asn"] == 13335 and ip["prefix"] == "104.16.0.0/13"
    assert ip["as_name"] == "CLOUDFLARENET - Cloudflare, Inc."
    state = run_and_wait(browser, tid, "ports", preset="web")
    assert "чужая инфраструктура" in state["text"] and "нечего сканировать" in state["summary"]
    assert assets(ctx, tid, "port") == {}


def test_hostinfo_handles_missing_name(browser, ctx, dnsd):
    tid = add_target(browser, ctx, dnsd, "nope.test")
    state = run_and_wait(browser, tid, "hostinfo")
    assert state["status"] == "done" and "нет такого имени" in state["text"]


# --- открытые порты ----------------------------------------------------------------

def test_ports_find_services_banners_and_closing(browser, ctx, dnsd, monkeypatch):
    ssh = Service(greeter(b"SSH-2.0-OpenSSH_9.6\r\n")).__enter__()
    site = Service(http_app(lambda line, h: (200, {"Server": "nginx/1.25.3"}, b"ok"))).__enter__()
    db = Service(greeter(b"\x0b\x00\x00\x00\x0a8.0.36\x00abc")).__enter__()
    closed = Service(greeter(b"")).__enter__()
    closed.stop()  # порт гарантированно закрыт
    monkeypatch.setattr(portlib, "HTTP_PORTS", portlib.HTTP_PORTS | {site.port})
    monkeypatch.setattr(portlib, "RISKY", {**portlib.RISKY, db.port: "mysql"})
    monkeypatch.setattr(portlib, "SERVICES", {**portlib.SERVICES, ssh.port: "ssh", db.port: "mysql"})
    tid = add_target(browser, ctx, dnsd, "shop.test")
    ports = ",".join(str(p) for p in (ssh.port, site.port, db.port, closed.port))
    state = run_and_wait(browser, tid, "ports", preset="custom", custom=ports, timeout_ms=500, banners="1")
    assert state["status"] == "done", state["text"]
    found = assets(ctx, tid, "port")
    assert set(found) == {f"127.0.0.1:{p}" for p in (ssh.port, site.port, db.port)}
    assert found[f"127.0.0.1:{ssh.port}"]["banner"] == "SSH-2.0-OpenSSH_9.6"
    assert "nginx/1.25.3" in found[f"127.0.0.1:{site.port}"]["banner"]
    assert found[f"127.0.0.1:{db.port}"]["risky"] is True and "опасных 1" in state["summary"]
    assert "не должно смотреть наружу" in state["text"]
    page = browser.get(f"/targets/{tid}").text
    assert f'id="n-127-0-0-1-{db.port}"' in page and "SSH-2.0-OpenSSH_9.6" in page  # порт в структуре
    assert f"{db.port} mysql открыт в интернет" in page and "База данных принимает" in page  # замечание с советом
    assert '<a href="#issues">замечания' in page and '<a href="#tree">структура' in page  # оглавление

    ssh.stop()  # между запусками сервис выключили
    state = run_and_wait(browser, tid, "ports", preset="custom", custom=ports, timeout_ms=500, banners="1")
    assert assets(ctx, tid, "port")[f"127.0.0.1:{ssh.port}"]["state"] == "closed"
    assert "закрылось с прошлого раза: 1" in state["text"]
    page = browser.get(f"/targets/{tid}").text
    assert f"{ssh.port} ssh закрылся" in page and "закрылись" in page
    site.stop()
    db.stop()


def test_ports_rejects_bad_list(browser, ctx, dnsd):
    tid = add_target(browser, ctx, dnsd, "shop.test")
    state = run_and_wait(browser, tid, "ports", preset="custom", custom="22, 8o")
    assert state["status"] == "failed" and "список портов" in state["error"] and "8o" in state["error"]


def test_parse_ports():
    assert portlib.parse_ports("22, 80 443;8000-8002") == [22, 80, 443, 8000, 8001, 8002]
    for bad in ("0", "70000", "90-80", "abc"):
        with pytest.raises(ValueError):
            portlib.parse_ports(bad)
    assert len(portlib.TOP) == 100 and len(set(portlib.TOP)) == 100


# --- пинг и веб --------------------------------------------------------------------

def test_reach_ping_redirect_title_and_cert(browser, ctx, dnsd, monkeypatch, tmp_path):
    pair = self_signed(tmp_path, ["shop.test", "www.shop.test", "api.shop.test", "other.test"], days=10)
    if pair is None:
        pytest.skip("нет openssl")
    cert, key = pair
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(cert, key)
    html = "<html><head><title> Магазин  на тесте </title></head></html>".encode()
    secure = Service(http_app(lambda line, h: (200, {"Server": "nginx/1.25.3", "Content-Type": "text/html; charset=utf-8"},
                                               html)), tls=tls).__enter__()
    plain = Service(http_app(lambda line, h: (301, {"Location": f"https://{h['host'].split(':')[0]}:{secure.port}/"},
                                              b""))).__enter__()
    monkeypatch.setattr(recon.Reach, "HTTPS_PORT", secure.port)
    monkeypatch.setattr(recon.Reach, "HTTP_PORT", plain.port)
    monkeypatch.setattr(web, "verify_context", lambda: ssl.create_default_context(cafile=str(cert)))
    tid = add_target(browser, ctx, dnsd, "shop.test")
    state = run_and_wait(browser, tid, "reach", attempts=2, timeout=3)
    assert state["status"] == "done", state["text"]
    ip = assets(ctx, tid, "ip")["127.0.0.1"]
    assert ip["reachable"] is True and ip["rtt_port"] == secure.port and ip["loss"] == "0/2"
    url = assets(ctx, tid, "url")[f"https://shop.test:{secure.port}/"]
    assert url["status"] == 200 and url["title"] == "Магазин на тесте" and url["server"] == "nginx/1.25.3"
    cert_asset = assets(ctx, tid, "certificate")[f"shop.test:{secure.port}"]
    assert cert_asset["valid"] is True and cert_asset["issuer"] == "Perimeter Test CA" and cert_asset["days_left"] <= 10
    assert "api.shop.test" in assets(ctx, tid, "subdomain")
    assert "other.test" in assets(ctx, tid, "domain")  # чужое имя из сертификата → подсказка
    assert "301 → https" in state["text"] and "сертификат кончается" in state["summary"]
    page = browser.get(f"/targets/{tid}").text
    assert "Можно добавить" in page and "other.test" in page
    assert "уводит на https" in page and "«Магазин на тесте»" in page  # сводка
    assert "сертификат кончается через" in page and f'href="#n-127-0-0-1-{secure.port}"' in page  # замечание → дерево
    assert assets(ctx, tid, "url")[f"http://shop.test:{plain.port}/"]["to_https"] is True
    secure.stop()
    plain.stop()


def test_reach_reports_bad_certificate(browser, ctx, dnsd, monkeypatch, tmp_path):
    pair = self_signed(tmp_path, ["shop.test"], days=30)
    if pair is None:
        pytest.skip("нет openssl")
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(*pair)
    secure = Service(http_app(lambda line, h: (200, {}, b"<title>x</title>")), tls=tls).__enter__()
    monkeypatch.setattr(recon.Reach, "HTTPS_PORT", secure.port)
    monkeypatch.setattr(recon.Reach, "HTTP_PORT", 9)  # http не слушает
    tid = add_target(browser, ctx, dnsd, "shop.test")
    state = run_and_wait(browser, tid, "reach", attempts=1, timeout=2)
    assert state["status"] == "done"
    bad = assets(ctx, tid, "certificate")[f"shop.test:{secure.port}"]
    assert bad["valid"] is False and "self" in bad["error"]
    assert assets(ctx, tid, "url")[f"https://shop.test:{secure.port}/"]["status"] == 200  # страница всё равно получена
    secure.stop()


# --- соседи ------------------------------------------------------------------------

def test_neighbors_and_one_click_add(browser, ctx, dnsd):
    tid = add_target(browser, ctx, dnsd, "net.test")
    state = run_and_wait(browser, tid, "neighbors", size="28")
    assert state["status"] == "done", state["text"]
    found = assets(ctx, tid, "neighbor")
    assert found["198.51.100.11"]["ptr"] == "mail.net.test" and found["198.51.100.11"]["related"] is True
    assert found["198.51.100.12"]["related"] is False and "198.51.100.10" not in found
    assert "похоже, тоже ваш" in state["text"] and "похожих на ваши 1" in state["summary"]
    page = browser.get(f"/targets/{tid}").text
    assert "Можно добавить" in page and "mail.net.test" in page
    browser.post(f"/targets/{tid}/hosts", {"hosts": "198.51.100.11"})
    with ctx.db.session() as s:
        assert s.get(Target, tid).include == ["198.51.100.11"]
    assert "Можно добавить" not in browser.get(f"/targets/{tid}").text


# --- запустить всё -----------------------------------------------------------------

def test_run_all_goes_passive_first(browser, ctx, dnsd):
    tid = add_target(browser, ctx, dnsd, "cdn.test")
    r = browser.post(f"/targets/{tid}/run", {"module": "__all__"})
    assert r.status_code == 303
    with ctx.db.session() as s:
        order = [run.module_id for run in s.scalars(select(Run).where(Run.target_id == tid).order_by(Run.id))]
    assert order == ["hostinfo", "neighbors", "reach", "ports"]


@pytest.mark.parametrize("ip,internal", [
    ("10.1.2.3", True), ("192.168.0.1", True), ("100.64.1.1", True), ("127.0.0.2", True),
    ("::1", True), ("fd00::1", True), ("::ffff:172.16.5.5", True),
    ("8.8.8.8", False), ("198.51.100.10", False), ("2001:4860::8888", False),
])
def test_private_means_internal_network(ip, internal):
    assert netkit.private(ip) is internal


def test_fmt_ms():
    assert netkit.fmt_ms(0.4) == "<1 мс" and netkit.fmt_ms(12.6) == "13 мс"


@pytest.mark.parametrize("raw,org", [
    ("CLOUDFLARENET - Cloudflare, Inc.", "Cloudflare, Inc."), ("BEGET-AS", "BEGET-AS"), (None, ""), ("X - ", "X - "),
])
def test_org_name(raw, org):
    assert netkit.org_name(raw) == org


def test_view_tree_issues_and_summary():
    """Страница цели из готовых находок: группы имён, порты с ответом сайта, замечания без повторов."""
    from types import SimpleNamespace

    from perimeter.core.scope import Scope
    from perimeter.extensions.recon import view

    def a(kind, value, **data):
        return Asset(kind=kind, value=value, data=data)

    ip = "93.184.216.34"
    cert = dict(valid=True, issuer="R3", expires="2026-12-01", days_left=5, sans=["shop.test", "www.shop.test"])
    rows = [
        a("ip", ip, domains=["www.shop.test", "shop.test"], cnames=["shop.test"], asn=64500,
          as_name="EX-NET - Example Hosting", country="NL", rtt_ms=0.4),
        a("port", f"{ip}:3306", ip=ip, port=3306, service="mysql", risky=True, state="open", banner="MySQL 8.0"),
        a("port", f"{ip}:443", ip=ip, port=443, service="https", state="open"),
        a("url", "https://shop.test/", status=200, title="Магазин", server="nginx/1.25.3", hsts=False),
        a("url", "http://shop.test/", status=301, to_https=True, location="https://shop.test/"),
        a("certificate", "shop.test:443", **cert),
        a("certificate", "www.shop.test:443", **cert),
        a("subdomain", "api.shop.test", source="сертификат", seen_on="shop.test"),
        a("dns_record", "NS ns1.beget.com", type="NS", host="ns1.beget.com", provider="Beget"),
    ]
    target = SimpleNamespace(domain="shop.test", name="shop.test", scope=Scope.build("shop.test", [], []))
    f = view.Facts(target, rows)

    root = view.tree(f)
    names, loose, dns = root.children
    assert [v for v, _ in names.items] == ["shop.test", "www.shop.test"] and not names.meta  # cname внутри группы не нужен
    node = names.children[0]
    assert node.id == "n-93-184-216-34" and ("Example Hosting", "") in node.meta and ("пинг <1 мс", "dim") in node.meta
    p80, p443, p3306 = node.children
    assert [p.items[0] for p in (p80, p443, p3306)] == [(f"{ip}:80", "80"), (f"{ip}:443", "443"), (f"{ip}:3306", "3306")]
    assert ("→ https", "ok") in p80.meta
    assert ("«Магазин»", "") in p443.meta and ("tls 5 дн", "err") in p443.tags
    assert ("наружу", "err") in p3306.tags and ("MySQL 8.0", "dim") in p3306.meta
    assert loose.children[0].items == [("api.shop.test", "api.shop.test")]
    assert dns.children[0].meta[:2] == [("Beget", ""), ("ns1.beget.com", "dim")]

    found = view.issues(f)
    assert found[0].title == "3306 mysql открыт в интернет" and found[0].href == "#n-93-184-216-34-3306"
    certs = [i for i in found if i.title.startswith("сертификат")]
    assert len(certs) == 1 and certs[0].where == "shop.test  www.shop.test" and certs[0].level == "err"
    assert [i.level for i in found] == sorted((i.level for i in found), key=["err", "warn", "info"].index)

    groups = {g.title: g for g in view.summary(f)}
    facts = {x.key: x for g in groups.values() for x in g.facts}
    assert facts["опасные"].value == [("3306 mysql", "err")] and facts["с http"].value == [("уводит на https", "ok")]
    assert facts["хостинг"].value[0] == ("Example Hosting", "") and facts["dns"].value == [("Beget", "")]
