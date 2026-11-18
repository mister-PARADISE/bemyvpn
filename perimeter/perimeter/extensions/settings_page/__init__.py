"""Настройки: страница собирается из разделов, которые объявили ядро и расширения."""

from __future__ import annotations

from fastapi import APIRouter, HTTPException, Request

from ...core.context import Context, Page
from ...web import actor, flash, form_data, get_ctx, redirect, render

TITLE = "Настройки"
DESCRIPTION = "Формы настроек строятся из описаний полей"
VERSION = "1.0"

router = APIRouter()


def _section_or_404(ctx: Context, section_id: str):
    section = ctx.settings.sections.get(section_id)
    if section is None:
        raise HTTPException(404, "Раздел настроек не найден")
    return section


def _page(request: Request, section, errors: dict, values: dict | None = None, status: int = 200):
    ctx = get_ctx(request)
    full = {fld.key: f"{section.id}.{fld.key}" for fld in section.fields}
    return render(request, "settings/index.html", status,
                  groups=ctx.settings.groups(), section=section,
                  values=values if values is not None else ctx.settings.values(section.id),
                  is_set={key: ctx.settings.is_set(path) for key, path in full.items()},
                  errors=errors)


@router.get("/settings")
def settings_root(request: Request):
    first = next(iter(get_ctx(request).settings.sections), None)
    if first is None:
        raise HTTPException(404, "Настроек нет")
    return redirect(f"/settings/{first.id}")


@router.get("/settings/{section_id}")
def settings_section(request: Request, section_id: str):
    return _page(request, _section_or_404(get_ctx(request), section_id), {})


@router.post("/settings/{section_id}")
async def settings_save(request: Request, section_id: str):
    ctx = get_ctx(request)
    section = _section_or_404(ctx, section_id)
    data = await form_data(request)
    result = ctx.settings.save_form(section_id, data)
    if not result.ok:
        values = ctx.settings.values(section_id)
        values.update({k: data.get(k) for k in result.errors})
        flash(request, "Не сохранено: проверьте отмеченные поля", "error")
        return _page(request, section, result.errors, values, 422)
    if result.changed:
        labels = {fld.key: fld.label for fld in section.fields}
        summary = ", ".join(f"{labels[k]}: {old} → {new}" for k, (old, new) in result.changed.items())
        with ctx.db.session() as db:
            ctx.audit(db, "settings.updated", f"Настройки «{section.title}»: {summary}", actor=actor(request),
                      data={"section": section_id, "changed": {labels[k]: list(v) for k, v in result.changed.items()}})
        restart = any(fld.restart for fld in section.fields if fld.key in result.changed)
        flash(request, "Сохранено" + (". Часть изменений заработает после перезапуска" if restart else ""))
    else:
        flash(request, "Ничего не изменилось", "info")
    return redirect(f"/settings/{section_id}")


def setup(ctx: Context) -> None:
    ctx.pages.add(Page("settings", "Настройки", "/settings", icon="sliders", placement="footer", order=100,
                       description="Название, оформление, часовой пояс, безопасность"))
    ctx.add_router(router)
