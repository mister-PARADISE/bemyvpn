"""Базы уязвимостей: NVD — все CVE продукта, CISA KEV — какие эксплуатируются в атаках, EPSS — насколько
вероятна атака в ближайший месяц.

Ходим не к цели, а в открытые базы, — поэтому модуль «Уязвимости» пассивный. Берём из NVD сразу всё,
что есть по продукту (сотни записей), а версию сверяем у себя: так находятся и точные совпадения,
и диапазоны «все версии до 9.8». Ответы лежат в data/vulndb/ — десятая цель с nginx не спрашивает
NVD заново, пока кеш свежий.
"""

from __future__ import annotations

import asyncio
import html
import json
import logging
import re
import time
from pathlib import Path
from typing import Any, Callable

import httpx

from . import versions

log = logging.getLogger("perimeter.vulndb")

NVD_PAGE = 2000  # больше NVD за раз не отдаёт
_REF_RANK = {"Exploit": 0, "Vendor Advisory": 1, "Patch": 2, "Mitigation": 3, "Release Notes": 4, "Third Party Advisory": 5}

# NVD без ключа: 5 запросов за 30 секунд на всех; с ключом — 50. Пауза общая для всех запусков.
NVD_GAP = {"key": 0.7, "free": 6.5}
_nvd_lock = asyncio.Lock()
_nvd_last = 0.0


class VulnDBError(Exception):
    """База не ответила или ответила не тем — понятная причина для консоли."""


def severity(score: float | None) -> str:
    if score is None:
        return "unknown"
    return "critical" if score >= 9 else "high" if score >= 7 else "medium" if score >= 4 else "low" if score > 0 else "none"


SEVERITY_RU = {"critical": "критическая", "high": "высокая", "medium": "средняя", "low": "низкая",
               "none": "нет", "unknown": "без оценки"}
SEVERITY_MANY = {"critical": "критических", "high": "высоких", "medium": "средних", "low": "низких",
                 "none": "неопасных", "unknown": "без оценки"}  # «критических 2, высоких 3»


def best_cvss(metrics: dict[str, Any]) -> tuple[float | None, str, str]:
    """(оценка, вектор, версия CVSS): v3.1 от NVD, потом от автора записи, потом v4.0, v3.0, v2."""
    for key, ver in (("cvssMetricV31", "3.1"), ("cvssMetricV40", "4.0"), ("cvssMetricV30", "3.0"), ("cvssMetricV2", "2.0")):
        rows = metrics.get(key) or []
        if not rows:
            continue
        rows = sorted(rows, key=lambda r: r.get("type") != "Primary")
        data = rows[0].get("cvssData") or {}
        if data.get("baseScore") is not None:
            return float(data["baseScore"]), data.get("vectorString", ""), ver
    return None, "", ""


def compact(cve: dict[str, Any], vp: str, target_sw: str = "*") -> dict[str, Any] | None:
    """Запись NVD → то, что нужно панели. Совпадения версий — только по этому продукту."""
    matches = []
    for conf in cve.get("configurations") or []:
        for node in conf.get("nodes") or []:
            for m in node.get("cpeMatch") or []:
                if not m.get("vulnerable"):
                    continue
                cm = versions.compact_match(m)
                if cm["vp"] != vp:  # у CVE бывают и другие продукты: «nginx на такой-то ОС»
                    continue
                if target_sw != "*" and cm["tsw"] not in (target_sw, "*", "-"):
                    continue
                matches.append(cm)
    if not matches:
        return None
    score, vector, cvss_version = best_cvss(cve.get("metrics") or {})
    refs = cve.get("references") or []
    top = sorted(refs, key=lambda r: min((_REF_RANK.get(t, 9) for t in r.get("tags") or []), default=9))[:6]
    return {
        "id": cve["id"],
        "published": (cve.get("published") or "")[:10],
        "status": cve.get("vulnStatus", ""),
        "summary": next((d["value"] for d in cve.get("descriptions") or [] if d.get("lang") == "en"), "")[:800],
        "cvss": score,
        "severity": severity(score),
        "vector": vector,
        "cvss_version": cvss_version,
        "cwe": sorted({d["value"] for w in cve.get("weaknesses") or [] for d in w.get("description") or []
                       if str(d.get("value", "")).startswith("CWE-")}),
        "exploit": any("Exploit" in (r.get("tags") or []) for r in refs),
        "kev": {"added": cve["cisaExploitAdd"], "name": cve.get("cisaVulnerabilityName", ""),
                "action": cve.get("cisaRequiredAction", "")} if cve.get("cisaExploitAdd") else None,
        "refs": [{"url": r["url"], "tags": r.get("tags") or []} for r in top],
        "matches": matches,
    }


_WP_OP = {("min", "gt"): "se", ("min", "ge"): "si", ("max", "lt"): "ee", ("max", "le"): "ei", ("max", "eq"): "v"}
_WP_SEV = {"c": "critical", "h": "high", "m": "medium", "l": "low", "n": "none"}
_ID = re.compile(r"^[A-Z]+-\d{4}-\d+$")


def wp_compact(v: dict[str, Any], slug: str) -> dict[str, Any]:
    """Запись WPVulnerability (данные Wordfence, Patchstack, WPScan) → тот же вид, что и у NVD."""
    op = v.get("operator") or {}
    match: dict[str, Any] = {"vp": f"wordpress:{slug}", "v": "*", "u": "*", "tsw": "wordpress"}
    for side in ("min", "max"):
        ver, how = op.get(f"{side}_version"), op.get(f"{side}_operator")
        if ver and how and (side, how) in _WP_OP:
            match[_WP_OP[(side, how)]] = ver
    sources = v.get("source") or []
    cve = next((x["id"] for x in sources if str(x.get("id", "")).startswith("CVE-")), None)
    impact = v.get("impact") or {}
    cvss = next((impact[k] for k in ("cvss", "cvss3", "cvss4") if (impact.get(k) or {}).get("score")),
                impact.get("cvss") or {})
    ssvc = impact.get("ssvc") or {}
    try:
        score = float(cvss["score"]) or None  # 0.0 у них — «не оценили»
    except (KeyError, TypeError, ValueError):
        score = None
    desc = next((str(x["description"]) for x in sources if x.get("description")), "")
    desc = html.unescape(desc[5:] if desc.startswith("[en] ") else desc).strip()
    dates = sorted(str(x["date"]) for x in sources if x.get("date"))
    # «Contact Form 7 < 5.3.2 - Unrestricted File Upload» — у записей Wordfence и WPScan; у CVE имя = номер
    named = next((html.unescape(str(x["name"])) for x in sources
                  if x.get("name") and x.get("name") != x.get("id") and not _ID.match(str(x["name"]))), "")
    return {
        "id": cve or f"WPV-{str(v.get('uuid', ''))[:12]}",
        "published": dates[0] if dates else "",
        "status": "",
        "title": named,
        "summary": (named + ". " if named else "") + desc[:700],
        "cvss": score,
        "severity": severity(score) if score is not None else _WP_SEV.get(str(cvss.get("severity", "")), "unknown"),
        "vector": cvss.get("vector", ""),
        "cvss_version": cvss.get("version", ""),
        "cwe": [c["cwe"] for c in impact.get("cwe") or [] if c.get("cwe")],
        "exploit": ssvc.get("exploitation") in ("poc", "active"),
        "kev": {"added": ssvc.get("kev_date") or "", "name": "", "action": "", "ransomware": False}
               if ssvc.get("kev") else None,
        "unfixed": op.get("unfixed") == "1",
        "refs": [{"url": x["link"], "tags": []} for x in sources if x.get("link")][:6],
        "matches": [match],
    }


class VulnDB:
    """Кеш и клиенты баз. get(ключ настройки) — значение из «Настроек → Уязвимости»."""

    def __init__(self, cache_dir: Path, get: Callable[[str], Any], say: Callable[[str, str], None] | None = None) -> None:
        self.dir = Path(cache_dir)
        self.get = get
        self.say = say or (lambda text, level="dim": None)
        self.requests = 0
        self.nvd_down = ""  # NVD не ответила и после повторов — остальные программы не ждут её зря

    # --- общее ---------------------------------------------------------------------
    def _fresh(self, path: Path) -> Any | None:
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return None
        if time.time() - data.get("fetched", 0) > float(self.get("vulndb.cache_hours")) * 3600:
            return None
        return data

    def _save(self, path: Path, data: dict[str, Any]) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix(".tmp")
        tmp.write_text(json.dumps({**data, "fetched": time.time()}, ensure_ascii=False), encoding="utf-8")
        tmp.replace(path)

    def client(self) -> httpx.AsyncClient:
        return httpx.AsyncClient(timeout=90, trust_env=True, follow_redirects=True,
                                 headers={"User-Agent": "Perimeter vulnerability lookup"})

    # --- NVD -----------------------------------------------------------------------
    async def _nvd_get(self, client: httpx.AsyncClient, params: dict[str, Any]) -> dict[str, Any]:
        global _nvd_last
        if self.nvd_down:
            raise VulnDBError(f"NVD недоступна ({self.nvd_down}) — пропускаю")
        key = self.get("vulndb.nvd_api_key") or ""
        gap = NVD_GAP["key" if key else "free"]
        for attempt in range(4):
            async with _nvd_lock:
                wait = _nvd_last + gap - time.monotonic()
                if wait > 0:
                    await asyncio.sleep(wait)
                _nvd_last = time.monotonic()
                self.requests += 1
                try:
                    resp = await client.get(self.get("vulndb.nvd_url"), params=params,
                                            headers={"apiKey": key} if key else {})
                except httpx.HTTPError as exc:
                    err = f"NVD не ответила: {type(exc).__name__}: {exc}"[:200]
                    resp = None
            if resp is not None and resp.status_code == 200:
                try:
                    return resp.json()
                except ValueError:
                    err = "NVD прислала не JSON"
            elif resp is not None:
                err = f"NVD ответила {resp.status_code}" + (" — лимит запросов, жду" if resp.status_code in (403, 429, 503) else "")
                if resp.status_code not in (403, 429, 500, 502, 503, 504):
                    raise VulnDBError(err)
            self.say(f"    [~] {err}", "warn")
            if attempt < 3:
                await asyncio.sleep(10 * (attempt + 1))
        self.nvd_down = err
        raise VulnDBError(err)

    async def product(self, client: httpx.AsyncClient, vp: str, target_sw: str = "*") -> list[dict[str, Any]]:
        """Все CVE продукта (vendor:product) из NVD — из кеша, если он свежий."""
        safe = re.sub(r"[^a-z0-9._-]+", "_", f"{vp}__{target_sw}".lower())
        path = self.dir / "nvd" / f"{safe}.json"
        cached = self._fresh(path)
        if cached is not None:
            return cached["items"]
        vendor, product = vp.split(":", 1)
        match = f"cpe:2.3:a:{vendor}:{product}:*:*:*:*:*:{target_sw}:*:*"
        items: list[dict[str, Any]] = []
        start, total = 0, None
        while total is None or start < total:
            data = await self._nvd_get(client, {"virtualMatchString": match, "resultsPerPage": NVD_PAGE,
                                                "startIndex": start, "noRejected": ""})
            total = int(data.get("totalResults") or 0)
            page = data.get("vulnerabilities") or []
            for item in page:
                c = compact(item.get("cve") or {}, vp, target_sw)
                if c is not None:
                    items.append(c)
            if not page:
                break
            start += len(page)
            if total > NVD_PAGE:
                self.say(f"    … {vp}: загружено {min(start, total)} из {total}", "dim")
        self._save(path, {"vp": vp, "total": total or 0, "items": items})
        return items

    # --- плагины и темы WordPress ----------------------------------------------------
    async def wordpress(self, client: httpx.AsyncClient, kind: str, slug: str) -> dict[str, Any]:
        """Уязвимости плагина (kind=plugin) или темы (theme) по названию из каталога WordPress.
        В NVD у плагинов чужие имена (Contact Form 7 — rocklobster:contact_form_7), а у многих свежих CVE
        их нет вовсе, — поэтому отдельная база, которая знает плагины по названию.
        Ответ: {"items": записи как у NVD, "name": название, "closed": причина, если плагин убрали из каталога}."""
        safe = re.sub(r"[^a-z0-9._-]+", "_", f"{kind}__{slug}".lower())
        path = self.dir / "wordpress" / f"{safe}.json"
        cached = self._fresh(path)
        if cached is not None:
            return cached["result"]
        self.requests += 1
        try:
            resp = await client.get(f"{str(self.get('vulndb.wp_url')).rstrip('/')}/{kind}/{slug}/")
        except httpx.HTTPError as exc:
            raise VulnDBError(f"база WordPress не ответила: {type(exc).__name__}") from None
        result: dict[str, Any] = {"items": [], "name": "", "closed": ""}
        if resp.status_code not in (200, 404):
            raise VulnDBError(f"база WordPress ответила {resp.status_code}")
        if resp.status_code == 200:
            try:
                body = resp.json()
            except ValueError:
                raise VulnDBError("база WordPress прислала не JSON") from None
            data = (body.get("data") if isinstance(body, dict) and not body.get("error") else None) or {}
            if not isinstance(data, dict):
                data = {}
            merged: dict[str, dict[str, Any]] = {}
            for v in data.get("vulnerability") or []:
                c = wp_compact(v, slug)
                if c["id"] in merged:  # одна CVE описана несколькими записями — сводим диапазоны
                    merged[c["id"]]["matches"] += c["matches"]
                else:
                    merged[c["id"]] = c
            closed = str(data.get("closed_reason") or "закрыт") if str(data.get("closed") or "0") not in ("0", "") else ""
            result = {"items": list(merged.values()), "name": str(data.get("name") or ""), "closed": closed}
        self._save(path, {"result": result})
        return result

    # --- CISA KEV ------------------------------------------------------------------
    async def kev(self, client: httpx.AsyncClient) -> dict[str, dict[str, Any]]:
        """Уязвимости, которые уже используют в атаках (каталог CISA KEV)."""
        path = self.dir / "kev.json"
        cached = self._fresh(path)
        if cached is not None:
            return cached["items"]
        try:
            resp = await client.get(self.get("vulndb.kev_url"))
            resp.raise_for_status()
            feed = resp.json()
        except (httpx.HTTPError, ValueError) as exc:
            self.say(f"    [~] каталог CISA KEV недоступен ({type(exc).__name__}) — признак «в атаках» только из NVD", "warn")
            return {}
        items = {v["cveID"]: {"added": v.get("dateAdded", ""), "name": v.get("vulnerabilityName", ""),
                              "action": v.get("requiredAction", ""),
                              "ransomware": v.get("knownRansomwareCampaignUse") == "Known"}
                 for v in feed.get("vulnerabilities") or [] if v.get("cveID")}
        self._save(path, {"items": items})
        return items

    # --- EPSS ----------------------------------------------------------------------
    async def epss(self, client: httpx.AsyncClient, ids: list[str]) -> dict[str, tuple[float, float]]:
        """Вероятность эксплуатации в ближайшие 30 дней (FIRST EPSS) и её процентиль."""
        path = self.dir / "epss.json"
        cached = self._fresh(path) or {"items": {}}
        known: dict[str, list[float]] = cached["items"]
        need = [i for i in dict.fromkeys(ids) if i not in known]
        for n in range(0, len(need), 100):
            chunk = need[n:n + 100]
            try:
                resp = await client.get(self.get("vulndb.epss_url"), params={"cve": ",".join(chunk)})
                resp.raise_for_status()
                for row in resp.json().get("data") or []:
                    known[row["cve"]] = [float(row.get("epss") or 0), float(row.get("percentile") or 0)]
                for cid in chunk:
                    known.setdefault(cid, [0.0, 0.0])
            except (httpx.HTTPError, ValueError, KeyError) as exc:
                self.say(f"    [~] EPSS недоступен ({type(exc).__name__}) — без вероятности атаки", "warn")
                break
        if need:
            self._save(path, {"items": known})
        return {i: (known[i][0], known[i][1]) for i in ids if i in known}
