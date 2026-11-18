from __future__ import annotations

import re
import warnings
from pathlib import Path

import pytest

warnings.filterwarnings("ignore", category=DeprecationWarning)

from fastapi.testclient import TestClient  # noqa: E402

from perimeter.config import Config  # noqa: E402
from perimeter.web.app import create_app  # noqa: E402

PASSWORD = "secret-pass-1"


class Browser:
    """Тестовый клиент, который ведёт себя как браузер: берёт CSRF-токен со страницы."""

    def __init__(self, client: TestClient) -> None:
        self.client = client
        self.csrf = ""

    def get(self, url: str, **kw):
        r = self.client.get(url, **kw)
        m = re.search(r'data-csrf="([^"]+)"', r.text)
        if m:
            self.csrf = m.group(1)
        return r

    def post(self, url: str, data: dict | None = None, **kw):
        if not self.csrf:
            self.get("/login")
        payload = {"_csrf": self.csrf, **(data or {})}
        kw.setdefault("follow_redirects", False)
        return self.client.post(url, data=payload, **kw)

    def json_post(self, url: str, body: dict):
        if not self.csrf:
            self.get("/")
        return self.client.post(url, json=body, headers={"X-CSRF-Token": self.csrf})


def make_app(tmp_path: Path, **overrides):
    # debug: упавший блок страницы роняет тест, а не прячется за заглушкой
    return create_app(Config(data_dir=tmp_path / "data", **{"debug": True, **overrides}))


@pytest.fixture
def app(tmp_path):
    app = make_app(tmp_path)
    yield app
    app.state.ctx.call_hooks("app.stop", app.state.ctx)  # остановить поток движка


@pytest.fixture
def anon(app) -> Browser:
    return Browser(TestClient(app))


@pytest.fixture
def browser(app) -> Browser:
    b = Browser(TestClient(app))
    b.get("/setup")
    r = b.post("/setup", {"username": "admin", "password": PASSWORD, "password2": PASSWORD})
    assert r.status_code == 303, r.text
    b.get("/")
    return b


@pytest.fixture
def ctx(app):
    return app.state.ctx


def run_and_wait(browser: Browser, target_id: int, module: str, timeout: float = 30, **params) -> dict:
    """Запустить модуль через панель и дождаться конца. Возвращает итог с полным выводом."""
    import time
    from urllib.parse import parse_qs, urlsplit

    form = {"module": module, **({"with_params": "1"} if params else {}), **{f"p_{k}": v for k, v in params.items()}}
    r = browser.post(f"/targets/{target_id}/run", form)
    assert r.status_code == 303, r.text
    run_id = int(parse_qs(urlsplit(r.headers["location"]).query)["run"][0])
    return wait_run(browser, run_id, timeout)


def wait_run(browser: Browser, run_id: int, timeout: float = 30) -> dict:
    import time

    lines, after, deadline = [], 0, time.monotonic() + timeout
    while True:
        state = browser.client.get(f"/runs/{run_id}/log", params={"after": after}).json()
        lines += state["lines"]
        if state["lines"]:
            after = state["lines"][-1]["n"]
        if not state["active"]:
            state["lines"] = lines
            state["text"] = "\n".join(ln["text"] for ln in lines)
            return state
        assert time.monotonic() < deadline, f"запуск {run_id} не закончился: {lines[-5:]}"
        time.sleep(0.1)


def flashes(page: str) -> str:
    """Тексты всплывающих уведомлений страницы — так, как их покажет браузер."""
    import html
    import json
    import re

    m = re.search(r"data-flashes='([^']*)'", page)
    return " | ".join(f["text"] for f in json.loads(html.unescape(m.group(1)))) if m else ""
