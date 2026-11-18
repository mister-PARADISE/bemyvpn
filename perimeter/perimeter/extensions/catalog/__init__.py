"""Каталог: какие модули подключены, какими данными они обмениваются, какие расширения загружены."""

from __future__ import annotations

from fastapi import APIRouter, Request

from ...core.context import Context, Page
from ...core.modules import Kind
from ...web import get_ctx, render

TITLE = "Каталог модулей"
DESCRIPTION = "Модули-инструменты, типы данных и загруженные расширения"
VERSION = "1.0"

router = APIRouter()

TABS = (("modules", "Модули"), ("types", "Типы данных"), ("extensions", "Расширения"))


@router.get("/modules")
def catalog(request: Request, tab: str = "modules"):
    ctx = get_ctx(request)
    if tab not in dict(TABS):
        tab = "modules"
    modules = list(ctx.modules)
    counts = {"modules": len(modules), "types": len(ctx.asset_types), "extensions": len(ctx.extensions)}
    types = []
    for t in ctx.asset_types:
        types.append({
            "type": t,
            "producers": [m for m in modules if t.id in m.spec.produces],
            "consumers": [m for m in modules if t.id in m.spec.consumes],
        })
    return render(request, "catalog/index.html", tab=tab, tabs=[(k, v, counts[k]) for k, v in TABS],
                  categories=ctx.modules.categories(), types=types, extensions=ctx.extensions, Kind=Kind,
                  query={"tab": tab})


def setup(ctx: Context) -> None:
    ctx.pages.add(Page("modules", "Модули", "/modules", icon="puzzle", section="Инструменты", order=60,
                       description="Инструменты проверки и как они связаны"))
    ctx.add_router(router)
