"""Программы и их уязвимости: версии, отпечатки, сопоставление с базами — на настоящих записях NVD и базы
WordPress, но без интернета (tests/data — выборка, tests/netlab.py — поддельные NVD, KEV, EPSS и WPVulnerability)."""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from sqlalchemy import select

from perimeter.extensions.recon import fingerprints as fp, ports as portlib, versions, vulndb
from perimeter.extensions.runs.models import Asset

from .conftest import run_and_wait
from .netlab import FakeDNS, Service, greeter, http_app, vulndb_app

DATA = Path(__file__).parent / "data"


# --- версии ------------------------------------------------------------------------

@pytest.mark.parametrize("a,b,expected", [
    ("9.6p1", "9.6", 1), ("9.6p1", "9.7p1", -1), ("9.8", "9.7p1", 1), ("1.0.2k", "1.0.2u", -1), ("1.0.2k", "1.0.2", 1),
    ("1.1.1", "1.0.2k", 1), ("2.0.0-rc1", "2.0.0", -1), ("2.0.0rc1", "2.0.0", -1), ("1.24", "1.24.0", 0),
    ("8.0.36", "8.0.4", 1), ("9.4.43.v20210629", "9.4.44", -1), ("2.4.58", "2.4.6", 1), ("6.4.2", "6.4", 1),
    ("2022.83", "2020.81", 1), ("10.6.12", "10.6.2", 1), ("3.5.0-beta1", "3.5.0", -1),
])
def test_compare(a, b, expected):
    assert versions.compare(a, b) == expected
    assert versions.compare(b, a) == -expected


def test_cpe_matching_exact_range_and_update():
    rng = versions.compact_match({"vulnerable": True, "criteria": "cpe:2.3:a:openbsd:openssh:*:*:*:*:*:*:*:*",
                                  "versionStartIncluding": "8.5", "versionEndExcluding": "9.8"})
    assert versions.affected(rng, "9.6p1") == (True, "range")
    assert not versions.affected(rng, "9.8p1")[0] and not versions.affected(rng, "8.4p1")[0]
    exact = versions.compact_match({"vulnerable": True, "criteria": "cpe:2.3:a:openbsd:openssh:9.6:p1:*:*:*:*:*:*"})
    assert versions.affected(exact, "9.6p1")[0] and not versions.affected(exact, "9.6")[0]
    any_update = versions.compact_match({"vulnerable": True, "criteria": "cpe:2.3:a:openbsd:openssh:9.6:*:*:*:*:*:*:*"})
    assert versions.affected(any_update, "9.6p1")[0] and versions.affected(any_update, "9.6")[0]
    assert versions.fixed_in([rng], "9.6p1") == "9.8"
    assert versions.split_cpe("cpe:2.3:a:joomla:joomla\\!:4.0.0:*:*:*:*:*:*:*")[4] == "joomla\\!"
    assert versions.precise("6.4.2") and not versions.precise("10")


# --- отпечатки ---------------------------------------------------------------------

def _hits(hits):
    return {(h.product, h.version) for h in hits}


@pytest.mark.parametrize("banner,product,version,distro,package", [
    ("SSH-2.0-OpenSSH_9.6p1 Ubuntu-3ubuntu13", "openssh", "9.6p1", "Ubuntu", "3ubuntu13"),
    ("SSH-2.0-OpenSSH_9.2p1 Debian-2+deb12u3", "openssh", "9.2p1", "Debian", "2+deb12u3"),
    ("SSH-2.0-dropbear_2022.83", "dropbear", "2022.83", "", ""),
    ("MySQL 8.0.36-0ubuntu0.22.04.1", "mysql", "8.0.36", "Ubuntu", "0ubuntu0.22.04.1"),
    ("MySQL 5.5.5-10.6.12-MariaDB-0ubuntu0.22.04.1", "mariadb", "10.6.12", "Ubuntu", "0ubuntu0.22.04.1"),
    ("220 mx.example.com ESMTP Exim 4.96 Tue, 19 Nov 2026", "exim", "4.96", "", ""),
    ("220 ProFTPD 1.3.5e Server (Debian)", "proftpd", "1.3.5e", "Debian", ""),
    ("220 (vsFTPd 3.0.3)", "vsftpd", "3.0.3", "", ""),
    ("# Server\r\nredis_version:7.2.4\r\n", "redis", "7.2.4", "", ""),
    ("VERSION 1.6.21\r\n", "memcached", "1.6.21", "", ""),
])
def test_banners(banner, product, version, distro, package):
    h = fp.from_banner(banner)[0]
    assert (h.product, h.version, h.distro, h.package) == (product, version, distro, package)


def test_headers_cookies_and_pages():
    hits = fp.from_headers({"server": "Apache/2.4.58 (Ubuntu) OpenSSL/3.0.2 PHP/8.1.2",
                            "x-powered-by": "PHP/8.1.2-1ubuntu2.14", "x-jenkins": "2.401.3"}, [{"name": "PHPSESSID"}])
    assert {("apache", "2.4.58"), ("openssl", "3.0.2"), ("php", "8.1.2"), ("jenkins", "2.401.3")} <= _hits(hits)
    assert ("tomcat", "") in _hits(fp.from_headers({"server": "Apache-Coyote/1.1"}, []))
    assert ("apache", "") not in _hits(fp.from_headers({"server": "Apache-Coyote/1.1"}, []))
    html = ('<meta name="generator" content="WordPress 6.4.2"><script src="/js/jquery-1.12.4.min.js"></script>'
            '<script src="https://cdnjs.cloudflare.com/ajax/libs/twitter-bootstrap/4.0.0/js/bootstrap.min.js"></script>'
            '<link href="/wp-content/plugins/contact-form-7/includes/css/styles.css?ver=5.3.1">'
            '<link href="/wp-content/themes/astra/style.css?ver=4.5.2"><script src="https://unpkg.com/vue@2.6.10/dist/vue.js">')
    assert {("wordpress", "6.4.2"), ("jquery", "1.12.4"), ("bootstrap", "4.0.0"), ("vue", "2.6.10")} <= _hits(fp.from_body(html))
    assert fp.wp_assets(html) == ({"contact-form-7": "5.3.1"}, {"astra": "4.5.2"})
    assert ("nginx", "1.18.0") in _hits(fp.from_body("<hr><center>nginx/1.18.0</center>"))  # страница 404
    apache = fp.from_body("<address>Apache/2.4.41 (Ubuntu) Server at example.com Port 80</address>")[0]
    assert (apache.product, apache.version, apache.distro) == ("apache", "2.4.41", "Ubuntu")
    assert ("roundcube", "1.6.3") in _hits(fp.from_body('rcmail.set_env({"rcversion":10603})'))
    assert ("nextcloud", "27.1.3") in _hits(fp.from_path(
        "/status.php", '{"installed":true,"version":"27.1.3.2","versionstring":"27.1.3","productname":"Nextcloud"}'))
    assert ("elasticsearch", "7.17.9") in _hits(fp.from_path(
        "/", '{"version":{"number":"7.17.9"},"tagline":"You Know, for Search"}'))
    assert fp.from_path("/readme.html", "<h1>WordPress</h1> Version 4.9.8")[0].weight == 1  # мог остаться от старой
    plugin = fp.product("wp-plugin:contact-form-7")
    assert plugin.wp == "plugin" and not plugin.cpes and plugin.checkable  # NVD плагины по названию не знает


def test_every_product_has_valid_cpe_names():
    for p in fp.PRODUCTS.values():
        for vp in p.cpes:
            vendor, _, product = vp.partition(":")
            assert vendor and product and " " not in vp, p.id
    for pid, *_ in fp.HEADERS + [(p, None) for p, _ in fp.BANNERS + fp.BODY + fp.TITLES + fp.SCRIPTS + fp.GENERATORS]:
        assert fp.product(pid) is not None, pid


# --- целиком: сайт в лаборатории → программы → уязвимости ----------------------------

PAGE = (b'<html><head><title>Shop</title><meta name="generator" content="WordPress 6.4.2">'
        b'<script src="/wp-includes/js/jquery/jquery-1.12.4.min.js"></script>'
        b'<link rel="stylesheet" href="/wp-content/plugins/contact-form-7/includes/css/styles.css?ver=5.3.1">'
        b'</head><body>shop</body></html>')


def _site(server: dict):
    """Магазин на WordPress: nginx и PHP называют версии в заголовках, плагин — в readme.txt."""
    def route(line: str, headers: dict[str, str]):
        path = (line.split(" ") + ["", ""])[1]  # «Открытые порты» стучатся и без запроса
        head = {"Server": server["nginx"], "X-Powered-By": "PHP/7.3.5", "Content-Type": "text/html"}
        if path == "/":
            return 200, head, PAGE
        if path == "/wp-content/plugins/contact-form-7/readme.txt":
            return 200, {**head, "Content-Type": "text/plain"}, b"=== Contact Form 7 ===\nStable tag: 5.3.1\n"
        return 404, head, f"<html><body><hr><center>{server['nginx']}</center></body></html>".encode()
    return http_app(route)


@pytest.fixture
def vulnlab(browser, ctx, monkeypatch):
    nvd = json.loads((DATA / "nvd.json").read_text(encoding="utf-8"))
    nvd["openbsd:openssh"].append({  # запись без версий: «пакеты RHEL 4 и 5 за 2008 год» — к OpenSSH 9.3 не относится
        "id": "CVE-2008-3844", "published": "2008-08-27T20:41:00.000", "vulnStatus": "Modified",
        "descriptions": [{"lang": "en", "value": "Certain Red Hat Enterprise Linux (RHEL) 4 and 5 packages for OpenSSH"}],
        "metrics": {"cvssMetricV2": [{"type": "Primary", "cvssData": {"baseScore": 9.3, "vectorString": "AV:N/AC:M/Au:N/C:C/I:C/A:C"}}]},
        "configurations": [{"nodes": [{"operator": "OR", "negate": False, "cpeMatch": [
            {"vulnerable": True, "criteria": "cpe:2.3:a:openbsd:openssh:*:*:*:*:*:*:*:*", "matchCriteriaId": "0"}]}]}]})
    wpv = json.loads((DATA / "wpv_contact-form-7.json").read_text(encoding="utf-8"))
    hits: list[str] = []
    server = {"nginx": "nginx/1.18.0"}
    with FakeDNS({("shop.test", "A"): ["127.0.0.1"]}) as dnsd, \
            Service(vulndb_app(nvd, ["CVE-2019-11043", "CVE-2020-11023"], {"CVE-2019-11043": 0.94}, hits,
                               wp={"plugin/contact-form-7": wpv})) as vdb, \
            Service(_site(server)) as site, Service(greeter(b"SSH-2.0-OpenSSH_9.3p1 Ubuntu-1ubuntu3\r\n")) as ssh:
        monkeypatch.setattr(portlib, "HTTP_PORTS", portlib.HTTP_PORTS | {site.port})
        monkeypatch.setattr(portlib, "SERVICES", {**portlib.SERVICES, ssh.port: "ssh"})
        monkeypatch.setattr(vulndb, "NVD_GAP", {"key": 0, "free": 0})  # пауза нужна настоящей NVD, не поддельной
        base = f"http://127.0.0.1:{vdb.port}"
        for key, value in {"recon.dns_servers": [f"127.0.0.1:{dnsd.port}"], "recon.dns_timeout": 2,
                           "vulndb.nvd_url": f"{base}/rest/json/cves/2.0", "vulndb.kev_url": f"{base}/kev.json",
                           "vulndb.epss_url": f"{base}/epss", "vulndb.wp_url": base}.items():
            ctx.settings.set(key, value)
        r = browser.post("/targets/new", {"domain": "shop.test"})
        tid = int(r.headers["location"].rsplit("/", 1)[1])
        state = run_and_wait(browser, tid, "ports", preset="custom", custom=f"{site.port},{ssh.port}",
                             timeout_ms=500, banners="1")
        assert state["status"] == "done", state["text"]
        yield {"tid": tid, "hits": hits, "server": server, "site": site, "ssh": ssh}


def _of(ctx, tid, kind):
    with ctx.db.session() as s:
        return {a.value: dict(a.data) for a in s.scalars(select(Asset).where(Asset.target_id == tid, Asset.kind == kind))}


def test_versions_to_vulnerabilities_end_to_end(browser, ctx, vulnlab):
    tid, hits = vulnlab["tid"], vulnlab["hits"]
    state = run_and_wait(browser, tid, "cve", timeout=60)
    assert state["status"] == "done", state["text"]
    assert [m for m, _ in state["chain"]][-2:] == ["tech", "cve"]  # «Технологии» движок запустил сам

    techs = _of(ctx, tid, "technology")
    for name in ("nginx 1.18.0", "PHP 7.3.5", "WordPress 6.4.2", "jQuery 1.12.4", "OpenSSH 9.3p1",
                 "WordPress плагин contact-form-7 5.3.1"):
        assert name in techs, (name, sorted(techs))
    assert techs["OpenSSH 9.3p1"]["distro"] == "Ubuntu"

    vulns = _of(ctx, tid, "vulnerability")
    expected = {"CVE-2021-23017": "nginx", "CVE-2019-11043": "php", "CVE-2015-9251": "jquery",
                "CVE-2019-11358": "jquery", "CVE-2020-11022": "jquery", "CVE-2020-11023": "jquery",
                "CVE-2023-51385": "openssh", "CVE-2025-26465": "openssh", "CVE-2024-31210": "wordpress",
                "CVE-2020-35489": "wp-plugin:contact-form-7", "CVE-2023-6449": "wp-plugin:contact-form-7",
                "CVE-2024-2242": "wp-plugin:contact-form-7"}
    for cve, product in expected.items():
        assert vulns.get(cve, {}).get("product") == product, cve
    # мимо: nginx до 1.17.7, плагин до 3.7.2 и с 6.2 — версии не те; повтор той же дыры без CVE — один раз
    assert not {"CVE-2019-20372", "CVE-2014-2265", "CVE-2026-114432"} & set(vulns)
    assert not [v for v in vulns if v.startswith("WPV-")]
    assert "CVE-2008-3844" not in vulns and "без указания версий («все версии»): 1" in state["text"]
    php = vulns["CVE-2019-11043"]
    assert php["kev"] and php["severity"] == "critical" and php["epss"] == pytest.approx(0.94)
    assert php["fixed_in"] == "7.3.11" and php["how"] == "range"
    cf7 = vulns["CVE-2020-35489"]
    assert cf7["cvss"] == 10.0 and cf7["fixed_in"] == "5.3.2" and "Unrestricted File Upload" in cf7["summary"]
    assert techs["PHP 7.3.5"]["kev"] == 1 and techs["PHP 7.3.5"]["upgrade_to"] == "7.3.11"
    assert techs["jQuery 1.12.4"]["upgrade_to"] == "3.5.0"  # закрыть все известные, не только опасные
    assert techs["WordPress плагин contact-form-7 5.3.1"]["upgrade_to"] == "5.9.2"
    assert techs["WordPress плагин contact-form-7 5.3.1"]["cve_source"] == "WordPress"
    assert state["summary"].startswith("уязвимостей 12: критических 2, высоких 3, в атаках 2")
    assert "критических 1, высоких 1, средних 1" in state["text"]
    assert "сверяйте с бюллетенями" in state["text"]  # OpenSSH из сборки Ubuntu: исправления могли перенести

    page = browser.get(f"/targets/{tid}").text  # раздел «Уязвимости», замечания, оглавление
    assert 'id="vulns"' in page and '<a href="#vulns">уязвимости' in page
    assert "https://nvd.nist.gov/vuln/detail/CVE-2019-11043" in page and "в атаках" in page
    assert "обновить до 7.3.11" in page and ">94%<" in page  # EPSS своей колонкой
    assert "PHP 7.3.5: уязвимостей" in page  # замечание по программе

    asked = len(hits)
    again = run_and_wait(browser, tid, "cve", timeout=30)
    assert again["status"] == "done" and len(hits) == asked  # второй раз — из кеша, базы не спрашивали
    assert "запросов к базам 0" in again["summary"]


def test_upgrade_closes_vulnerabilities(browser, ctx, vulnlab):
    tid = vulnlab["tid"]
    assert run_and_wait(browser, tid, "cve", timeout=60)["status"] == "done"
    assert _of(ctx, tid, "vulnerability")["CVE-2021-23017"]["state"] == "open"
    vulnlab["server"]["nginx"] = "nginx/1.25.3"  # nginx обновили
    assert run_and_wait(browser, tid, "tech", timeout=60)["status"] == "done"
    state = run_and_wait(browser, tid, "cve", timeout=60)
    assert state["status"] == "done", state["text"]
    techs, vulns = _of(ctx, tid, "technology"), _of(ctx, tid, "vulnerability")
    assert techs["nginx 1.18.0"]["state"] == "gone" and techs["nginx 1.25.3"]["state"] == "seen"
    assert vulns["CVE-2021-23017"]["state"] == "closed" and vulns["CVE-2019-11043"]["state"] == "open"
    assert "закрыто с прошлого раза: 1" in state["text"]


def test_wordpress_source_and_dedupe():
    wpv = json.loads((DATA / "wpv_contact-form-7.json").read_text(encoding="utf-8"))
    items = [vulndb.wp_compact(v, "contact-form-7") for v in wpv["data"]["vulnerability"]]
    by_id = {c["id"]: c for c in items}
    c = by_id["CVE-2020-35489"]
    assert c["title"] == "Contact Form 7 < 5.3.2 - Unrestricted File Upload"  # &lt; раскрыт
    assert c["matches"][0]["ee"] == "5.3.2" and c["severity"] == "critical"
    later = by_id["CVE-2026-114432"]["matches"][0]
    assert later["si"] == "6.2" and later["ee"] == "6.2.4"  # «>= 6.2 и < 6.2.4»
    assert not versions.affected(later, "5.3.1")[0] and versions.affected(later, "6.2.3")[0]
    from perimeter.extensions.recon.modules import _dedupe, _kind
    assert _kind("WordPress Contact Form 7 plugin <= 5.3.1 - Unrestricted File Upload vulnerability") == \
        _kind("Contact Form 7 < 5.3.2 - Unrestricted File Upload") == "unrestricted file upload"
    assert _kind("WooCommerce Plugin <= 8.1.1 is vulnerable to Cross Site Scripting (XSS)") == "cross site scripting"
    hit = [(x, "range") for x in items if any(versions.affected(m, "5.3.1")[0] for m in x["matches"])]
    kept = {x["id"] for x, _ in _dedupe(hit, "5.3.1")}
    assert "CVE-2020-35489" in kept and not [i for i in kept if i.startswith("WPV-")]
    unfixed = vulndb.wp_compact({"operator": {"max_version": "7.1.0", "max_operator": "eq", "unfixed": "1"},
                                 "source": [{"id": "CVE-2022-50972", "name": "CVE-2022-50972"}],
                                 "impact": {"cvss": {"score": "0.0", "severity": "c"}}}, "woocommerce")
    assert unfixed["unfixed"] and unfixed["matches"][0]["v"] == "7.1.0" and unfixed["cvss"] is None
    assert unfixed["severity"] == "critical"  # оценки нет, но уровень база указала
    assert versions.affected(unfixed["matches"][0], "7.1.0")[0] and not versions.affected(unfixed["matches"][0], "7.1.1")[0]


def test_upgrade_text():
    from perimeter.extensions.recon import view
    assert view.upgrade_text({"upgrade_to": "8.0.43"}) == "8.0.43"
    # «уязвимы до 8.0.44 включительно»: номер с исправлением база не называет — любая новее
    assert view.upgrade_text({"upgrade_to": "8.0.44", "upgrade_after": True}) == "версии новее 8.0.44"
    assert view.upgrade_text({}) == ""
