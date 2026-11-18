"""Клиенты и счета: кто заказчик, как с ним связаться, что выставлено и что оплачено.

Оплата ручная: вы выставляете счёт, клиент платит, вы отмечаете счёт
оплаченным — и «Оплачено до» у клиента продлевается само.
"""

from __future__ import annotations

import math
import re
from collections import defaultdict
from datetime import date, timedelta
from decimal import Decimal

from fastapi import APIRouter, HTTPException, Request
from sqlalchemy import func, or_, select

from ...core import fields as f
from ...core.context import Context, Page, SearchHit, Widget
from ...web import DB, actor, flash, form_data, get_ctx, redirect, render
from ...web.ui import CURRENCIES, money
from .models import CLIENT_STATUSES, INVOICE_STATUSES, Client, Invoice

TITLE = "Клиенты и счета"
DESCRIPTION = "Карточки клиентов, контакты, ручные счета и сроки оплаты"
VERSION = "1.0"
PER_PAGE = 50

CLIENT_FIELDS = (
    f.text("name", "Имя или название", required=True, max_length=160, width="half",
           placeholder="Иван Петров или ООО «Ромашка»"),
    f.text("company", "Компания", max_length=160, width="half"),
    f.text("telegram", "Telegram", max_length=100, width="half", placeholder="@username"),
    f.email("email", "Почта", max_length=200, width="half"),
    f.text("phone", "Телефон", max_length=50, width="half"),
    f.choice("status", "Статус", {k: v[0] for k, v in CLIENT_STATUSES.items()}, default="lead", width="half"),
    f.date("paid_until", "Оплачено до", width="half",
           help="Продлевается само, когда отмечаете счёт оплаченным"),
    f.textarea("notes", "Заметки", help="Видите только вы"),
)

INVOICE_FIELDS = (
    f.text("title", "За что", required=True, max_length=300, placeholder="Аудит внешнего периметра site.ru"),
    f.decimal("amount", "Сумма", required=True, min=0, max=10**9, width="half"),
    f.choice("currency", "Валюта", CURRENCIES, width="half"),
    f.date("issued_on", "Дата счёта", width="half"),
    f.date("due_on", "Оплатить до", width="half"),
    f.date("covers_until", "Продлевает обслуживание до", width="half",
           help="После оплаты «Оплачено до» у клиента станет не раньше этой даты"),
    f.text("number", "Номер", max_length=40, width="half", help="Пусто — присвоится автоматически"),
)

router = APIRouter()


# --- помощники ---------------------------------------------------------------

def normalize_telegram(value: str | None) -> str:
    text = (value or "").strip()
    text = re.sub(r"^(https?://)?(t\.me|telegram\.me)/", "", text, flags=re.I)
    return text.lstrip("@").strip("/")


def _client_or_404(db, client_id: int) -> Client:
    client = db.get(Client, client_id)
    if client is None:
        raise HTTPException(404, "Клиент не найден")
    return client


def _invoice_or_404(db, invoice_id: int) -> Invoice:
    invoice = db.get(Invoice, invoice_id)
    if invoice is None:
        raise HTTPException(404, "Счёт не найден")
    return invoice


def next_invoice_number(db, on: date | None = None) -> str:
    year = (on or date.today()).year
    prefix = f"{year}-"
    taken = set(db.scalars(select(Invoice.number).where(Invoice.number.like(f"{prefix}%"))))
    n = len(taken) + 1
    while f"{prefix}{n:03d}" in taken:
        n += 1
    return f"{prefix}{n:03d}"


def totals_by_currency(invoices) -> list[tuple[str, Decimal]]:
    sums: dict[str, Decimal] = defaultdict(Decimal)
    for inv in invoices:
        sums[inv.currency] += inv.amount
    return sorted(sums.items(), key=lambda kv: -kv[1])


def _client_values(values: dict) -> dict:
    values["telegram"] = normalize_telegram(values.get("telegram"))
    for key in ("company", "email", "phone", "notes", "telegram"):
        values[key] = values.get(key) or ""
    return values


def _diff(obj, values: dict, fields) -> dict:
    changed = {}
    for fld in fields:
        if fld.key in values and getattr(obj, fld.key) != values[fld.key]:
            changed[fld.label] = [fld.display(getattr(obj, fld.key)), fld.display(values[fld.key])]
    return changed


# --- клиенты -----------------------------------------------------------------

@router.get("/clients")
def clients_list(request: Request, db: DB, q: str = "", status: str = "", page: int = 1):
    stmt = select(Client)
    if status in CLIENT_STATUSES:
        stmt = stmt.where(Client.status == status)
    elif status != "all":
        stmt = stmt.where(Client.status != "archived")
    if q.strip():
        like = f"%{q.strip()}%"
        stmt = stmt.where(or_(Client.name.ilike(like), Client.company.ilike(like), Client.email.ilike(like),
                              Client.telegram.ilike(like), Client.phone.ilike(like), Client.notes.ilike(like)))
    total = db.scalar(select(func.count()).select_from(stmt.subquery())) or 0
    pages = max(1, math.ceil(total / PER_PAGE))
    page = min(max(page, 1), pages)
    rows = db.scalars(stmt.order_by(Client.updated_at.desc()).offset((page - 1) * PER_PAGE).limit(PER_PAGE)).all()
    counts = dict(db.execute(select(Client.status, func.count()).group_by(Client.status)).all())
    tabs = [("", "Текущие", sum(v for k, v in counts.items() if k != "archived"))]
    tabs += [(k, v[0], counts.get(k, 0)) for k, v in CLIENT_STATUSES.items()]
    return render(request, "clients/list.html", clients=rows, q=q, status=status, tabs=tabs,
                  page=page, pages=pages, total=total, query={"q": q, "status": status})


@router.get("/clients/new")
def client_new(request: Request):
    return render(request, "clients/form.html", client=None, fields=CLIENT_FIELDS,
                  values=f.initial(CLIENT_FIELDS), errors={})


@router.post("/clients/new")
async def client_create(request: Request, db: DB):
    values, errors = f.parse_form(CLIENT_FIELDS, await form_data(request))
    if errors:
        return render(request, "clients/form.html", 422, client=None, fields=CLIENT_FIELDS, values=values,
                      errors=errors)
    client = Client(**_client_values(values))
    db.add(client)
    db.flush()
    get_ctx(request).audit(db, "client.created", f"Новый клиент: {client.name}", entity=("client", client.id),
                           actor=actor(request))
    flash(request, "Клиент добавлен")
    return redirect(f"/clients/{client.id}")


@router.get("/clients/{client_id}")
def client_detail(request: Request, db: DB, client_id: int):
    client = _client_or_404(db, client_id)
    ctx = get_ctx(request)
    open_invoices = [i for i in client.invoices if i.status == "issued"]
    paid = [i for i in client.invoices if i.status == "paid"]
    invoice_values = f.initial(INVOICE_FIELDS)
    invoice_values.update(currency=ctx.settings.get("general.currency"), issued_on=date.today(),
                          due_on=date.today() + timedelta(days=7))
    return render(request, "clients/detail.html", client=client, open_total=totals_by_currency(open_invoices),
                  paid_total=totals_by_currency(paid), invoice_fields=INVOICE_FIELDS,
                  invoice_values=invoice_values, statuses=CLIENT_STATUSES)


@router.get("/clients/{client_id}/edit")
def client_edit(request: Request, db: DB, client_id: int):
    client = _client_or_404(db, client_id)
    values = f.initial(CLIENT_FIELDS, client)
    if values.get("telegram"):
        values["telegram"] = "@" + values["telegram"]
    return render(request, "clients/form.html", client=client, fields=CLIENT_FIELDS, values=values, errors={})


@router.post("/clients/{client_id}/edit")
async def client_update(request: Request, db: DB, client_id: int):
    client = _client_or_404(db, client_id)
    values, errors = f.parse_form(CLIENT_FIELDS, await form_data(request))
    if errors:
        return render(request, "clients/form.html", 422, client=client, fields=CLIENT_FIELDS, values=values,
                      errors=errors)
    values = _client_values(values)
    changed = _diff(client, values, CLIENT_FIELDS)
    for key, value in values.items():
        setattr(client, key, value)
    if changed:
        get_ctx(request).audit(db, "client.updated", f"Изменён клиент {client.name}: {', '.join(changed)}",
                               entity=("client", client.id), data={"changed": changed}, actor=actor(request))
    flash(request, "Сохранено" if changed else "Ничего не изменилось", "success" if changed else "info")
    return redirect(f"/clients/{client.id}")


@router.post("/clients/{client_id}/status")
async def client_status(request: Request, db: DB, client_id: int):
    client = _client_or_404(db, client_id)
    status = (await form_data(request)).get("status")
    if status not in CLIENT_STATUSES or status == client.status:
        return redirect(f"/clients/{client.id}")
    old = client.status_title
    client.status = status
    get_ctx(request).audit(db, "client.status", f"{client.name}: {old} → {client.status_title}",
                           entity=("client", client.id), actor=actor(request))
    flash(request, f"Статус: {client.status_title}")
    return redirect(f"/clients/{client.id}")


@router.post("/clients/{client_id}/delete")
def client_delete(request: Request, db: DB, client_id: int):
    client = _client_or_404(db, client_id)
    ctx = get_ctx(request)
    blockers = [msg for msg in ctx.call_hooks("client.before_delete", db, client) if msg]
    if blockers:
        flash(request, " ".join(blockers), "error")
        return redirect(f"/clients/{client.id}")
    name = client.name
    count = len(client.invoices)
    db.delete(client)
    ctx.audit(db, "client.deleted", f"Удалён клиент {name} (счетов: {count})", actor=actor(request))
    flash(request, f"Клиент «{name}» удалён")
    return redirect("/clients")


# --- счета -------------------------------------------------------------------

@router.get("/invoices")
def invoices_list(request: Request, db: DB, status: str = "issued", page: int = 1):
    stmt = select(Invoice).join(Client)
    if status in INVOICE_STATUSES:
        stmt = stmt.where(Invoice.status == status)
    total = db.scalar(select(func.count()).select_from(stmt.subquery())) or 0
    pages = max(1, math.ceil(total / PER_PAGE))
    page = min(max(page, 1), pages)
    order = Invoice.due_on.asc().nulls_last() if status == "issued" else Invoice.id.desc()
    rows = db.scalars(stmt.order_by(order, Invoice.id.desc()).offset((page - 1) * PER_PAGE).limit(PER_PAGE)).all()
    counts = dict(db.execute(select(Invoice.status, func.count()).group_by(Invoice.status)).all())
    tabs = [(k, v[0], counts.get(k, 0)) for k, v in INVOICE_STATUSES.items()] + [("all", "Все", sum(counts.values()))]
    issued = db.scalars(select(Invoice).where(Invoice.status == "issued")).all()
    month_start = date.today().replace(day=1)
    paid_month = db.scalars(select(Invoice).where(Invoice.status == "paid", Invoice.paid_on >= month_start)).all()
    return render(request, "clients/invoices.html", invoices=rows, status=status, tabs=tabs, page=page,
                  pages=pages, total=total, query={"status": status},
                  open_total=totals_by_currency(issued),
                  overdue_total=totals_by_currency([i for i in issued if i.is_overdue]),
                  paid_month=totals_by_currency(paid_month))


@router.post("/clients/{client_id}/invoices")
async def invoice_create(request: Request, db: DB, client_id: int):
    client = _client_or_404(db, client_id)
    values, errors = f.parse_form(INVOICE_FIELDS, await form_data(request))
    number = (values.get("number") or "").strip()
    if not errors and number and db.scalar(select(Invoice.id).where(Invoice.number == number)):
        errors["number"] = "Такой номер уже есть"
    if errors:
        flash(request, "Счёт не создан: " + "; ".join(
            f"{next(x.label for x in INVOICE_FIELDS if x.key == k)} — {v}" for k, v in errors.items()), "error")
        return redirect(f"/clients/{client.id}#invoices")
    values["number"] = number or next_invoice_number(db, values.get("issued_on"))
    values["issued_on"] = values.get("issued_on") or date.today()
    invoice = Invoice(client=client, status="issued", **values)
    db.add(invoice)
    db.flush()
    get_ctx(request).audit(
        db, "invoice.created", f"Счёт №{invoice.number} на {money(invoice.amount, invoice.currency)} — {client.name}",
        entity=("client", client.id), data={"invoice": invoice.number}, actor=actor(request))
    flash(request, f"Счёт №{invoice.number} выставлен")
    return redirect(f"/clients/{client.id}#invoices")


@router.get("/invoices/{invoice_id}/edit")
def invoice_edit(request: Request, db: DB, invoice_id: int):
    invoice = _invoice_or_404(db, invoice_id)
    return render(request, "clients/invoice_form.html", invoice=invoice, fields=INVOICE_FIELDS,
                  values=f.initial(INVOICE_FIELDS, invoice), errors={})


@router.post("/invoices/{invoice_id}/edit")
async def invoice_update(request: Request, db: DB, invoice_id: int):
    invoice = _invoice_or_404(db, invoice_id)
    values, errors = f.parse_form(INVOICE_FIELDS, await form_data(request))
    number = (values.get("number") or "").strip()
    if not number:
        errors["number"] = "У счёта должен быть номер"
    elif number != invoice.number and db.scalar(select(Invoice.id).where(Invoice.number == number)):
        errors["number"] = "Такой номер уже есть"
    if errors:
        return render(request, "clients/invoice_form.html", 422, invoice=invoice, fields=INVOICE_FIELDS,
                      values=values, errors=errors)
    values["number"] = number
    values["issued_on"] = values.get("issued_on") or invoice.issued_on
    changed = _diff(invoice, values, INVOICE_FIELDS)
    for key, value in values.items():
        setattr(invoice, key, value)
    if changed:
        get_ctx(request).audit(db, "invoice.updated", f"Изменён счёт №{invoice.number}: {', '.join(changed)}",
                               entity=("client", invoice.client_id), data={"changed": changed}, actor=actor(request))
    flash(request, "Счёт сохранён" if changed else "Ничего не изменилось", "success" if changed else "info")
    return redirect(f"/clients/{invoice.client_id}#invoices")


@router.post("/invoices/{invoice_id}/status")
async def invoice_status(request: Request, db: DB, invoice_id: int):
    invoice = _invoice_or_404(db, invoice_id)
    data = await form_data(request)
    to = data.get("to")
    back = data.get("back") if str(data.get("back", "")).startswith("/") else f"/clients/{invoice.client_id}#invoices"
    allowed = {"draft": {"issued"}, "issued": {"paid", "cancelled", "draft"}, "paid": {"issued"},
               "cancelled": {"issued"}}
    if to not in allowed.get(invoice.status, set()):
        flash(request, "Так статус счёта поменять нельзя", "error")
        return redirect(back)
    client = invoice.client
    note = ""
    invoice.status = to
    if to == "paid":
        invoice.paid_on = date.today()
        if invoice.covers_until and (client.paid_until is None or client.paid_until < invoice.covers_until):
            client.paid_until = invoice.covers_until
            note = f", обслуживание до {invoice.covers_until:%d.%m.%Y}"
        if client.status == "lead":
            client.status = "active"
    else:
        invoice.paid_on = None
    get_ctx(request).audit(
        db, f"invoice.{to}", f"Счёт №{invoice.number} ({client.name}): {invoice.status_title}{note}",
        entity=("client", client.id), data={"invoice": invoice.number}, actor=actor(request))
    flash(request, f"Счёт №{invoice.number}: {invoice.status_title.lower()}{note}")
    return redirect(back)


@router.post("/invoices/{invoice_id}/delete")
def invoice_delete(request: Request, db: DB, invoice_id: int):
    invoice = _invoice_or_404(db, invoice_id)
    client_id, number = invoice.client_id, invoice.number
    db.delete(invoice)
    get_ctx(request).audit(db, "invoice.deleted", f"Удалён счёт №{number}", entity=("client", client_id),
                           actor=actor(request))
    flash(request, f"Счёт №{number} удалён")
    return redirect(f"/clients/{client_id}#invoices")


# --- виджет и поиск ----------------------------------------------------------

def payments_widget(db, ctx: Context) -> dict:
    remind = ctx.settings.get("general.remind_days")
    horizon = date.today() + timedelta(days=remind)
    issued = db.scalars(select(Invoice).where(Invoice.status == "issued")).all()
    month_start = date.today().replace(day=1)
    paid_month = db.scalars(select(Invoice).where(Invoice.status == "paid", Invoice.paid_on >= month_start)).all()
    expiring = db.scalars(
        select(Client).where(Client.status == "active", Client.paid_until.is_not(None), Client.paid_until <= horizon)
        .order_by(Client.paid_until).limit(6)
    ).all()
    return {
        "open_total": totals_by_currency(issued),
        "open_count": len(issued),
        "overdue_count": sum(1 for i in issued if i.is_overdue),
        "paid_month": totals_by_currency(paid_month),
        "expiring": expiring,
    }


def search_clients(db, q: str) -> list[SearchHit]:
    like = f"%{q}%"
    hits = [
        SearchHit(c.name, f"/clients/{c.id}", "Клиенты", c.company or (f"@{c.telegram}" if c.telegram else c.email),
                  "user")
        for c in db.scalars(select(Client).where(or_(Client.name.ilike(like), Client.company.ilike(like),
                                                     Client.email.ilike(like), Client.telegram.ilike(like)))
                            .limit(8))
    ]
    hits += [
        SearchHit(f"Счёт №{i.number}", f"/clients/{i.client_id}#invoices", "Счета",
                  f"{i.client.name} · {money(i.amount, i.currency)}", "receipt")
        for i in db.scalars(select(Invoice).where(or_(Invoice.number.ilike(like), Invoice.title.ilike(like))).limit(5))
    ]
    return hits


def _open_invoices_badge(db) -> int:
    return db.scalar(select(func.count()).select_from(Invoice).where(Invoice.status == "issued")) or 0


def setup(ctx: Context) -> None:
    ctx.pages.add(Page("clients", "Клиенты", "/clients", icon="users", order=20,
                       description="Заказчики, контакты, статусы"))
    ctx.pages.add(Page("invoices", "Счета", "/invoices", icon="receipt", order=30, badge=_open_invoices_badge,
                       badge_tone="warning", description="Выставленные и оплаченные счета"))
    ctx.add_router(router)
    ctx.widgets.add(Widget("payments", "Оплаты", "clients/_widget.html", data=payments_widget, icon="wallet",
                           order=20, link="/invoices"))
    ctx.search(search_clients)
    ctx.entity("client", lambda id: f"/clients/{id}", "Клиенты")
    # Другие расширения могут запретить удаление клиента: ctx.hook("client.before_delete", fn) → текст причины.
