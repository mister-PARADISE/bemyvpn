"""Базы модулей «Уязвимости» и «Поддержка версий», которые панель обновляет сама (core/feeds.py).

Обновляется только нужное: программы, которые нашлись на целях. Десять сайтов на одном nginx —
одна запись в NVD, а не десять. NVD обновляется изменениями с прошлого раза, остальные — целиком.
"""

from __future__ import annotations

from typing import Any

from sqlalchemy import select

from ...core.feeds import Feed
from ..runs.models import Asset
from . import fingerprints as fp, versions, vulndb


def in_use(job) -> list[tuple[fp.Product, str]]:
    """Программы со всех целей: (продукт, версия) — без тех, что больше не видны."""
    out: dict[tuple[str, str], tuple[fp.Product, str]] = {}
    with job.ctx.db.session() as s:
        for data in s.scalars(select(Asset.data).where(Asset.kind == "technology")):
            data = data or {}
            prod = fp.product(data.get("product", ""))
            if prod is None or data.get("state") == "gone":
                continue
            out[(prod.id, data.get("version") or "")] = (prod, data.get("version") or "")
    return list(out.values())


def _db(job) -> vulndb.VulnDB:
    return vulndb.VulnDB(job.data_dir / "vulndb", job.setting, say=job.log)


async def _each(job, db: vulndb.VulnDB, items: list[Any], fetch, name) -> tuple[int, int]:
    """Обновить по списку; ошибка одного не мешает остальным. → (записей всего, ошибок)."""
    total = failed = 0
    async with db.client() as client:
        for item in items:
            try:
                got = await fetch(client, item)
            except vulndb.VulnDBError as exc:
                job.log(f"[!] {name(item)}: {exc}", "err")
                failed += 1
                continue
            total += len(got)
            job.log(f"[*] {name(item)}: записей {len(got)}", "dim")
    if items and failed == len(items):
        raise vulndb.VulnDBError("база не ответила ни по одной программе")
    return total, failed


async def refresh_nvd(job) -> str:
    wanted = sorted({(vp, prod.target_sw) for prod, version in in_use(job)
                     if prod.cpes and versions.precise(version) for vp in prod.cpes})
    if not wanted:
        job.log("[i] программ с точной версией на целях пока нет — обновлять нечего", "dim")
        return "программ нет"
    db = _db(job)
    total, failed = await _each(job, db, wanted, lambda c, it: db.product(c, it[0], it[1]), lambda it: it[0])
    job.items, job.changed = total, db.changed
    return (f"программ {len(wanted)}, записей {total}, изменилось {db.changed}"
            + (f", не ответила по {failed}" if failed else "") + f"; запросов {db.requests}")


async def refresh_wordpress(job) -> str:
    wanted = sorted({(prod.wp, prod.id.partition(":")[2]) for prod, _v in in_use(job) if prod.wp})
    if not wanted:
        job.log("[i] плагинов и тем WordPress на целях нет — обновлять нечего", "dim")
        return "плагинов нет"
    db = _db(job)

    async def fetch(client, item):
        return (await db.wordpress(client, item[0], item[1]))["items"]

    total, failed = await _each(job, db, wanted, fetch, lambda it: f"{it[0]} {it[1]}")
    job.items, job.changed = total, db.changed
    return f"плагинов и тем {len(wanted)}, записей {total}, изменилось {db.changed}" + (f", ошибок {failed}" if failed else "")


async def refresh_kev(job) -> str:
    db = _db(job)
    async with db.client() as client:
        items = await db.kev(client)
    if not items:
        raise vulndb.VulnDBError("каталог не загрузился")
    job.items, job.changed = len(items), db.changed
    return f"записей {len(items)}, новых {db.changed}"


async def refresh_epss(job) -> str:
    with job.ctx.db.session() as s:
        ids = sorted({v for v, data in s.execute(select(Asset.value, Asset.data).where(Asset.kind == "vulnerability"))
                      if v.startswith("CVE-") and (data or {}).get("state") != "closed"})
    if not ids:
        job.log("[i] уязвимостей на целях нет — пересчитывать нечего", "dim")
        return "уязвимостей нет"
    db = _db(job)
    async with db.client() as client:
        got = await db.epss(client, ids)
    job.items, job.changed = len(got), db.changed
    return f"оценок {len(got)}, заметно изменилось {db.changed}"


async def refresh_eol(job) -> str:
    wanted = sorted({prod.eol for prod, _v in in_use(job) if prod.eol})
    if not wanted:
        job.log("[i] программ, которые знает endoflife.date, на целях нет", "dim")
        return "программ нет"
    db = _db(job)
    total, failed = await _each(job, db, wanted, db.eol, lambda slug: slug)
    job.items, job.changed = total, db.changed
    return f"программ {len(wanted)}, веток {total}, изменилось {db.changed}" + (f", ошибок {failed}" if failed else "")


def register(ctx) -> None:
    every = lambda c: c.settings.get("vulndb.cache_hours")  # noqa: E731 — одно расписание на все базы
    common = {"every": every, "group": "Уязвимости"}
    ctx.feeds.add(Feed(
        "vulndb.nvd", "NVD", refresh_nvd, source="services.nvd.nist.gov", modules=("cve",), order=10,
        description="Все CVE программ, найденных на целях. Каждый день появляются новые записи и уточняются "
                    "диапазоны версий; качаются только изменения с прошлого раза.", **common))
    ctx.feeds.add(Feed(
        "vulndb.wordpress", "База WordPress", refresh_wordpress, source="wpvulnerability.net", modules=("cve",),
        order=20, description="Уязвимости плагинов и тем WordPress, найденных на целях: данные Wordfence, "
                              "Patchstack, WPScan.", **common))
    ctx.feeds.add(Feed(
        "vulndb.kev", "CISA KEV", refresh_kev, source="cisa.gov", modules=("cve",), order=30,
        description="Каталог уязвимостей, которые уже используют в настоящих атаках.", **common))
    ctx.feeds.add(Feed(
        "vulndb.epss", "EPSS", refresh_epss, source="first.org", modules=("cve",), order=40,
        description="Вероятность, что уязвимость начнут использовать в ближайший месяц, — пересчитывается "
                    "каждый день для всех уязвимостей на целях.", **common))
    ctx.feeds.add(Feed(
        "vulndb.eol", "Сроки поддержки", refresh_eol, source="endoflife.date", modules=("eol",), order=50,
        description="До какого числа ветки программ получают исправления безопасности и какой выпуск последний.",
        **common))
