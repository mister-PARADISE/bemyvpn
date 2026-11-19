"""Каталог: какие модули подключены, какими данными они обмениваются, какие расширения загружены."""

from __future__ import annotations

from fastapi import APIRouter, Request

from ...core.context import Context, Page, Stat
from ...core.modules import Kind
from ...web import get_ctx, render

TITLE = "Каталог модулей"
DESCRIPTION = "Модули-инструменты, типы данных и загруженные расширения"
VERSION = "1.0"

router = APIRouter()

TABS = (("modules", "Модули"), ("data", "Данные"), ("types", "Типы данных"), ("extensions", "Расширения"))


@router.get("/modules")
def catalog(request: Request, tab: str = "modules"):
    ctx = get_ctx(request)
    if tab not in dict(TABS):
        tab = "modules"
    modules = list(ctx.modules)
    counts = {"modules": len(modules), "data": len(ctx.feeds), "types": len(ctx.asset_types),
              "extensions": len(ctx.extensions)}
    types = []
    for t in ctx.asset_types:
        types.append({
            "type": t,
            "producers": [m for m in modules if t.id in m.spec.produces],
            "consumers": [m for m in modules if t.id in m.spec.consumes],
        })
    built_on = {m.spec.id: [d for d in modules if m.spec.id in d.spec.needs] for m in modules}
    feeds_of = {m.spec.id: [fd.title for fd in ctx.feeds if m.spec.id in fd.modules] for m in modules}
    return render(request, "catalog/index.html", tab=tab, tabs=[(k, v, counts[k]) for k, v in TABS],
                  categories=ctx.modules.categories(), types=types, extensions=ctx.extensions, Kind=Kind,
                  built_on=built_on, feeds_of=feeds_of, query={"tab": tab})


def modules_stat(db, ctx: Context) -> str:
    modules = list(ctx.modules)
    if not modules:
        return "пока нет"
    passive = sum(1 for m in modules if m.spec.kind == Kind.PASSIVE)
    return f"{passive} пассивных, {len(modules) - passive} активных"


def setup(ctx: Context) -> None:
    ctx.pages.add(Page("modules", "Модули", "/modules", icon="puzzle", section="Инструменты", order=14,
                       description="Инструменты проверки и как они связаны"))
    ctx.add_router(router)
    ctx.stats.add(Stat("modules", "модули", modules_stat, order=20, link="/modules"))
