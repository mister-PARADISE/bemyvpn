"""Модули разведки на настоящих сокетах и DNS, но без интернета (tests/netlab.py).

Один модуль — один вопрос; то, на чём модуль построен, движок запускает сам (state["chain"]).
"""

from __future__ import annotations

import ssl

import pytest
from sqlalchemy import select

from perimeter.extensions.recon import modules as recon, netkit, ports as portlib, web
from perimeter.extensions.runs.models import Asset, Run
from perimeter.extensions.targets.models import Target

from .conftest import run_and_wait, wait_chain
from .netlab import FakeDNS, Service, greeter, http_app, self_signed

CF_TXT = '"13335 | 104.16.0.0/13 | US | arin | 2014-03-28"'
CF_NAME = '"13335 | US | arin | 2010-07-14 | CLOUDFLARENET - Cloudflare, Inc., US"'
ROOT_KSK = ("257 3 8 AwEAAagAIKlVZrpC6Ia7gEzahOR+9W29euxhJhVVLOyQbSEW0O8gcCjFFVQUTf6v58fLjwBd0YI0EzrAcQqBGCzh/RStIoO8g0NfnfL2MTJRk"
            "xoXbfDaUeVPQuYEhg37NZWAJQ9VnMVDxP/VHL496M/QZxkjf5/Efucp2gaDX6RS6CXpoY68LsvPVjR0ZSwzz1apAzvN9dlzEheX7ICJBBtuA6G3LQpzW5h"
            "OA2hzCTMjJPJ8LbqF6dsV6DoBQzgul0sGIcGOYl7OyQdXfZ57relSQageu+ipAdTTJ25AsRTAoub8ONGcLmqrAmRLKBP1dfwhYB4N7knNnulqQxA+Uk1ihz0=")

ZONE = {
    # магазин: Яндекс, почта с SPF ~all и DMARC p=none, CAA только Let's Encrypt
    ("shop.test", "A"): ["127.0.0.1"],
    ("www.shop.test", "CNAME"): ["shop.test."],
    ("shop.test", "NS"): ["ns1.yandex.net.", "ns2.yandex.net."],
    ("shop.test", "SOA"): ["ns1.yandex.net. hostmaster.shop.test. 2026111801 3600 600 604800 300"],
    ("shop.test", "CAA"): ['0 issue "letsencrypt.org"'],
    ("shop.test", "MX"): ["10 mx.yandex.net."],
    ("shop.test", "TXT"): ['"v=spf1 include:_spf.yandex.net ~all"'],
    ("_dmarc.shop.test", "TXT"): ['"v=DMARC1; p=none; rua=mailto:d@shop.test"'],
    ("1.0.0.127.in-addr.arpa", "PTR"): ["localhost."],
    # сайт за Cloudflare, у которого две записи SPF (так не работает ни одна)
    ("cdn.test", "A"): ["104.16.1.1"],
    ("cdn.test", "NS"): ["ada.ns.cloudflare.com.", "bob.ns.cloudflare.com."],
    ("cdn.test", "TXT"): ['"v=spf1 -all"', '"v=spf1 include:mailgun.org ~all"'],
    ("1.1.16.104.origin.asn.cymru.com", "TXT"): [CF_TXT],
    ("AS13335.asn.cymru.com", "TXT"): [CF_NAME],
    # сайт у хостера, с соседями; зона подписана, почты нет
    ("net.test", "A"): ["198.51.100.10"],
    ("net.test", "NS"): ["ns1.beget.com.", "ns2.beget.com."],
    ("net.test", "DNSKEY"): [ROOT_KSK],
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


# --- DNS ------------------------------------------------------------------------------

def test_resolve_finds_addresses_and_notices_move(browser, ctx, dnsd):
    tid = add_target(browser, ctx, dnsd, "shop.test")
    state = run_and_wait(browser, tid, "resolve")
    assert state["chain"] == [("resolve", "done")] and state["summary"] == "1 адрес: 127.0.0.1"
    ip = assets(ctx, tid, "ip")["127.0.0.1"]
    assert set(ip["domains"]) == {"shop.test", "www.shop.test"} and ip["cnames"] == ["shop.test"] and ip["private"]
    assert "CNAME  shop.test" in state["text"]

    dnsd.zone[("shop.test", "A")] = ["127.0.0.2"]  # сайт переехал на другой сервер
    state = run_and_wait(browser, tid, "resolve")
    ips = assets(ctx, tid, "ip")
    assert ips["127.0.0.1"]["gone"] is True and ips["127.0.0.1"]["domains"] == []
    assert set(ips["127.0.0.1"]["was"]) == {"shop.test", "www.shop.test"} and ips["127.0.0.2"]["gone"] is False
    assert "больше не указывают: 1" in state["summary"] and "больше не трогают" in state["text"]


def test_resolve_handles_missing_name_and_dependents_say_why(browser, ctx, dnsd):
    tid = add_target(browser, ctx, dnsd, "nope.test")
    state = run_and_wait(browser, tid, "ping")
    assert state["chain"] == [("resolve", "done"), ("ping", "done")] and state["summary"] == "нет адресов"
    state = run_and_wait(browser, tid, "resolve")
    assert "нет такого имени" in state["text"] and state["summary"] == "имена не разрешаются"


def test_provider_detects_cdn_and_ports_skip_it(browser, ctx, dnsd):
    tid = add_target(browser, ctx, dnsd, "cdn.test")
    state = run_and_wait(browser, tid, "ports", preset="web")
    assert state["chain"] == [("resolve", "done"), ("asn", "done"), ("ports", "done")]
    ip = assets(ctx, tid, "ip")["104.16.1.1"]
    assert ip["cdn"] == "Cloudflare" and ip["asn"] == 13335 and ip["prefix"] == "104.16.0.0/13" and ip["checked"] is True
    assert ip["as_name"] == "CLOUDFLARENET - Cloudflare, Inc."
    assert "чужая инфраструктура" in state["text"] and "нечего сканировать" in state["summary"]
    assert assets(ctx, tid, "port") == {}
    # «Адреса» и «Провайдер» свежие — второй раз не повторяются
    assert run_and_wait(browser, tid, "ports", preset="web")["chain"] == [("ports", "done")]


def test_reverse_names(browser, ctx, dnsd):
    tid = add_target(browser, ctx, dnsd, "net.test")
    state = run_and_wait(browser, tid, "ptr")
    assert state["chain"] == [("resolve", "done"), ("ptr", "done")] and state["summary"] == "имена у 1 из 1"
    assert assets(ctx, tid, "ip")["198.51.100.10"]["ptr"] == "web10.isp.example"


def test_zone_ns_soa_dnssec_caa(browser, ctx, dnsd):
    tid = add_target(browser, ctx, dnsd, "shop.test")
    state = run_and_wait(browser, tid, "zone")
    rec = assets(ctx, tid, "dns_record")
    assert rec["NS ns1.yandex.net"]["provider"] == "Яндекс"
    assert rec["SOA shop.test"]["email"] == "hostmaster@shop.test" and rec["SOA shop.test"]["serial"] == "2026111801"
    assert rec["DNSSEC shop.test"]["signed"] is False and rec["CAA shop.test"]["issuers"] == ["letsencrypt.org"]
    assert state["summary"] == "DNS Яндекс, DNSSEC нет, CAA есть"

    tid = add_target(browser, ctx, dnsd, "net.test")
    state = run_and_wait(browser, tid, "zone")
    rec = assets(ctx, tid, "dns_record")
    assert rec["DNSSEC net.test"]["signed"] is True and rec["CAA net.test"]["issuers"] == []
    assert state["summary"] == "DNS Beget, DNSSEC есть, CAA нет"


def test_mail_mx_spf_dmarc(browser, ctx, dnsd):
    tid = add_target(browser, ctx, dnsd, "shop.test")
    state = run_and_wait(browser, tid, "mail")
    rec = assets(ctx, tid, "dns_record")
    assert rec["MX 10 mx.yandex.net"]["provider"] == "Яндекс 360"
    assert rec["SPF shop.test"]["all"] == "~all" and rec["DMARC shop.test"]["policy"] == "none"
    assert state["summary"] == "почта Яндекс 360, SPF ~all, DMARC p=none"

    tid = add_target(browser, ctx, dnsd, "net.test")
    assert run_and_wait(browser, tid, "mail")["summary"] == "почта не принимается, SPF нет, DMARC нет"

    tid = add_target(browser, ctx, dnsd, "cdn.test")
    state = run_and_wait(browser, tid, "mail")
    assert "SPF сломан" in state["summary"] and "должна быть одна" in state["text"]


@pytest.mark.parametrize("record,expected", [
    ("v=spf1 -all", "-all"), ("v=spf1 include:_spf.x ~all", "~all"), ("v=spf1 all", "+all"),
    ("v=spf1 ?all", "?all"), ("v=spf1 redirect=_spf.x", "redirect=_spf.x"), ("v=spf1 mx", ""),
])
def test_spf_all(record, expected):
    assert recon.spf_all(record) == expected


# --- соседи ------------------------------------------------------------------------

def test_neighbors_and_one_click_add(browser, ctx, dnsd):
    tid = add_target(browser, ctx, dnsd, "net.test")
    state = run_and_wait(browser, tid, "neighbors", size="28")
    assert state["chain"] == [("resolve", "done"), ("asn", "done"), ("neighbors", "done")], state["text"]
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
    assert state["chain"] == [("resolve", "done"), ("asn", "done"), ("ports", "done")]
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
    listing = browser.get("/targets").text  # список целей: адрес и замечания у каждого сайта
    assert "127.0.0.1" in listing and "частная сеть" in listing and "[!] 1" in listing
    assert "серьёзных 1 на 1 цели" in browser.get("/").text  # строка «Панели» на обзоре
    hits = browser.client.get("/api/search", params={"q": "127.0.0.1"}).json()["items"]
    assert any(i["kind"] == "Найдено" and "shop.test" in i["subtitle"] for i in hits)  # «где этот IP?»
    with ctx.db.session() as s:
        run_id = s.scalars(select(Run.id).where(Run.module_id == "ports")).first()
    assert "Одновременно подключений" in browser.get(f"/runs/{run_id}").text  # параметры словами, а не ключами

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


# --- пинг, ответ сайта, сертификат -------------------------------------------------------

def test_ping_site_and_certificate(browser, ctx, dnsd, monkeypatch, tmp_path):
    pair = self_signed(tmp_path, ["shop.test", "www.shop.test", "api.shop.test", "other.test"], days=10)
    if pair is None:
        pytest.skip("нет openssl")
    cert, key = pair
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(cert, key)
    html = "<html><head><title> Магазин  на тесте </title></head></html>".encode()
    secure = Service(http_app(lambda line, h: (200, {"Server": "nginx/1.25.3", "Set-Cookie": "sid=1; HttpOnly",
                                                      "Content-Type": "text/html; charset=utf-8"}, html)), tls=tls).__enter__()
    plain = Service(http_app(lambda line, h: (301, {"Location": f"https://{h['host'].split(':')[0]}:{secure.port}/"},
                                              b""))).__enter__()
    monkeypatch.setattr(recon.Ping, "PORTS", (secure.port, plain.port))
    monkeypatch.setattr(recon.Site, "HTTPS_PORT", secure.port)
    monkeypatch.setattr(recon.Site, "HTTP_PORT", plain.port)
    monkeypatch.setattr(recon.Certificate, "PORT", secure.port)
    monkeypatch.setattr(web, "verify_context", lambda: ssl.create_default_context(cafile=str(cert)))
    tid = add_target(browser, ctx, dnsd, "shop.test")

    state = run_and_wait(browser, tid, "ping", attempts=2, timeout=3)
    assert state["chain"] == [("resolve", "done"), ("ping", "done")]
    ip = assets(ctx, tid, "ip")["127.0.0.1"]
    assert ip["reachable"] is True and ip["rtt_port"] == secure.port and ip["loss"] == "0/2"

    state = run_and_wait(browser, tid, "http", timeout=3)
    urls = assets(ctx, tid, "url")
    page = urls[f"https://shop.test:{secure.port}/"]
    assert page["status"] == 200 and page["title"] == "Магазин на тесте" and page["server"] == "nginx/1.25.3"
    assert "CSP" in page["missing"] and page["cookies"] == [{"name": "sid", "secure": False, "httponly": True, "samesite": ""}]
    assert urls[f"http://shop.test:{plain.port}/"]["to_https"] is True
    assert "301 → https" in state["text"] and "cookie без флага Secure: sid" in state["text"]

    state = run_and_wait(browser, tid, "tls", timeout=3)
    cert_asset = assets(ctx, tid, "certificate")[f"shop.test:{secure.port}"]
    assert cert_asset["valid"] is True and cert_asset["issuer"] == "Perimeter Test CA" and cert_asset["days_left"] <= 10
    assert cert_asset["protocol"].startswith("TLSv1.") and cert_asset["bits"] >= 128
    assert "api.shop.test" in assets(ctx, tid, "subdomain")
    assert "other.test" in assets(ctx, tid, "domain")  # чужое имя из сертификата → подсказка
    assert "кончается через" in state["summary"]

    page = browser.get(f"/targets/{tid}").text
    assert "Можно добавить" in page and "other.test" in page
    assert "уводит на https" in page and "«Магазин на тесте»" in page  # сводка
    assert "сертификат кончается через" in page and f'href="#n-127-0-0-1-{secure.port}"' in page  # замечание → дерево
    assert "cookie без флага Secure" in page
    secure.stop()
    plain.stop()


def test_certificate_explains_bad_cert(browser, ctx, dnsd, monkeypatch, tmp_path):
    pair = self_signed(tmp_path, ["shop.test"], days=30)
    if pair is None:
        pytest.skip("нет openssl")
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(*pair)
    secure = Service(http_app(lambda line, h: (200, {}, b"<title>x</title>")), tls=tls).__enter__()
    monkeypatch.setattr(recon.Certificate, "PORT", secure.port)
    monkeypatch.setattr(recon.Site, "HTTPS_PORT", secure.port)
    monkeypatch.setattr(recon.Site, "HTTP_PORT", 9)  # http не слушает
    tid = add_target(browser, ctx, dnsd, "shop.test")
    state = run_and_wait(browser, tid, "tls", timeout=2)
    assert state["status"] == "done"
    bad = assets(ctx, tid, "certificate")[f"shop.test:{secure.port}"]
    assert bad["valid"] is False and bad["error"] == "самоподписанный: браузеры ему не верят" and "self" in bad["raw"]
    run_and_wait(browser, tid, "http", timeout=2)
    assert assets(ctx, tid, "url")[f"https://shop.test:{secure.port}/"]["status"] == 200  # страница всё равно получена
    assert "сертификат не проходит проверку" in browser.get(f"/targets/{tid}").text
    secure.stop()


# --- запустить всё -----------------------------------------------------------------

def test_run_all_is_one_chain_in_dependency_order_and_stops_whole(browser, ctx, dnsd):
    tid = add_target(browser, ctx, dnsd, "cdn.test")
    r = browser.post(f"/targets/{tid}/run", {"module": "__all__"})
    assert r.status_code == 303
    with ctx.db.session() as s:
        runs = list(s.scalars(select(Run).where(Run.target_id == tid).order_by(Run.id)))
    assert [run.module_id for run in runs] == ["resolve", "asn", "ptr", "zone", "mail", "neighbors",
                                               "ping", "http", "tls", "ports", "tech", "cve"]
    assert {run.chain for run in runs} == {runs[0].id}
    browser.post(f"/runs/{runs[0].id}/cancel")  # «Остановить всё»
    state = wait_chain(browser, runs[0].id, timeout=15)
    assert all(status in ("done", "cancelled") for _m, status in state["chain"])
    assert state["chain"][-1] == ("cve", "cancelled")


# --- мелочи ------------------------------------------------------------------------

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


@pytest.mark.parametrize("message,text", [
    ("certificate has expired", "сертификат истёк"),
    ("Hostname mismatch, certificate is not valid for 'x.test'.", "выдан на другое имя"),
    ("unable to get local issuer certificate", "неполная цепочка: сервер не отдаёт промежуточный сертификат"),
    ("что-то новое", "что-то новое"),
])
def test_verify_errors_are_explained(message, text):
    assert web.explain_verify(message) == text


def test_view_tree_issues_and_summary():
    """Страница цели из готовых находок: группы имён, порты с ответом сайта, замечания без повторов."""
    from types import SimpleNamespace

    from perimeter.core.scope import Scope
    from perimeter.extensions.recon import view

    def a(kind, value, **data):
        return Asset(kind=kind, value=value, data=data)

    ip = "93.184.216.34"
    cert = dict(valid=True, issuer="R3", expires="2026-12-01", days_left=5, sans=["shop.test", "www.shop.test"],
                protocol="TLSv1.3")
    rows = [
        a("ip", ip, domains=["www.shop.test", "shop.test"], cnames=["shop.test"], asn=64500, checked=True,
          as_name="EX-NET - Example Hosting", country="NL", rtt_ms=0.4),
        a("ip", "93.184.216.99", domains=[], gone=True, was=["old.shop.test"]),
        a("port", f"{ip}:3306", ip=ip, port=3306, service="mysql", risky=True, state="open", banner="MySQL 8.0"),
        a("port", f"{ip}:443", ip=ip, port=443, service="https", state="open"),
        a("url", "https://shop.test/", status=200, title="Магазин", server="nginx/1.25.3", hsts=False,
          missing=["CSP"], cookies=[{"name": "sid", "secure": False, "httponly": True, "samesite": ""}]),
        a("url", "http://shop.test/", status=301, to_https=True, location="https://shop.test/"),
        a("certificate", "shop.test:443", **cert),
        a("certificate", "www.shop.test:443", **cert),
        a("subdomain", "api.shop.test", source="сертификат", seen_on="shop.test"),
        a("dns_record", "NS ns1.beget.com", type="NS", host="ns1.beget.com", provider="Beget"),
        a("dns_record", "SPF shop.test", type="SPF", zone="shop.test", records=["v=spf1 +all"], all="+all"),
        a("dns_record", "DMARC shop.test", type="DMARC", zone="shop.test", records=[], policy="", detail="нет"),
        a("dns_record", "MX 10 mx.beget.com", type="MX", host="mx.beget.com", priority="10", provider="Beget"),
    ]
    target = SimpleNamespace(domain="shop.test", name="shop.test", scope=Scope.build("shop.test", [], []))
    f = view.Facts(target, rows)

    root = view.tree(f)
    names, gone, loose, dns = root.children
    assert [v for v, _ in names.items] == ["shop.test", "www.shop.test"] and not names.meta  # cname внутри группы не нужен
    assert gone.children[0].items == [("93.184.216.99", "93.184.216.99")] and gone.children[0].dim
    node = names.children[0]
    assert node.id == "n-93-184-216-34" and ("Example Hosting", "") in node.meta and ("пинг <1 мс", "dim") in node.meta
    p80, p443, p3306 = node.children
    assert [p.items[0] for p in (p80, p443, p3306)] == [(f"{ip}:80", "80"), (f"{ip}:443", "443"), (f"{ip}:3306", "3306")]
    assert ("→ https", "ok") in p80.meta
    assert ("«Магазин»", "") in p443.meta and ("tls 5 дн", "err") in p443.tags and ("TLSv1.3", "dim") in p443.meta
    assert ("наружу", "err") in p3306.tags and ("MySQL 8.0", "dim") in p3306.meta
    assert loose.children[0].items == [("api.shop.test", "api.shop.test")]
    assert [c.items[0][1] for c in dns.children] == ["ns", "mx", "spf", "dmarc"]

    found = view.issues(f)
    titles = [i.title for i in found]
    assert titles[0] == "3306 mysql открыт в интернет" and found[0].href == "#n-93-184-216-34-3306"
    certs = [i for i in found if i.title.startswith("сертификат")]
    assert len(certs) == 1 and certs[0].where == "shop.test  www.shop.test" and certs[0].level == "err"
    assert "SPF разрешает отправлять всем (+all)" in titles and "нет DMARC" in titles
    assert "cookie без флага Secure: sid" in titles and "нет защитных заголовков: CSP" in titles
    assert [i.level for i in found] == sorted((i.level for i in found), key=["err", "warn", "info"].index)

    facts = {x.key: x for g in view.summary(f) for x in g.facts}
    assert facts["опасные"].value == [("3306 mysql", "err")] and facts["с http"].value == [("уводит на https", "ok")]
    assert facts["хостинг"].value[0] == ("Example Hosting", "") and facts["dns"].value == [("Beget", "")]
    assert facts["spf"].value == [("+all", "err")] and facts["dmarc"].value == [("нет", "warn")]
    assert "93.184.216.99" in facts["адреса"].sub and facts["cookie"].value == [("без Secure: sid", "warn")]

    # модуль, который ещё не запускался, — кнопка вместо значения
    fresh = view.Facts(target, rows[:1], ran={"resolve": None, "asn": None})
    todo = {x.key: x.run for g in view.summary(fresh) for x in g.facts if x.run}
    assert todo["пинг"] == ("ping", "Пинг") and todo["сертификат"] == ("tls", "Сертификат") and "хостинг" not in todo
