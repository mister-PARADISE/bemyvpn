"""Запуск: python -m perimeter [run | reset-password ИМЯ]"""

from __future__ import annotations

import sys

if sys.version_info < (3, 11):  # понятное сообщение вместо трассировки — до импорта всего остального
    sys.exit(f"Периметру нужен Python 3.11 или новее, а запущен {sys.version.split()[0]}.\n"
             "Удалите папку .venv и запустите start.sh (start.bat) снова — скрипт найдёт подходящий Python.")

import argparse  # noqa: E402
import getpass  # noqa: E402
from dataclasses import replace  # noqa: E402
from pathlib import Path  # noqa: E402

from .config import Config  # noqa: E402


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="perimeter", description="Панель «Периметр»")
    parser.add_argument("--data", type=Path, help="папка с базой и плагинами (по умолчанию ./data)")
    sub = parser.add_subparsers(dest="command")
    run = sub.add_parser("run", help="запустить панель (по умолчанию)")
    run.add_argument("--host", help="адрес (по умолчанию 127.0.0.1 — только с этого компьютера)")
    run.add_argument("--port", type=int, help="порт (по умолчанию 8710)")
    run.add_argument("--reload", action="store_true", help="перезапуск при изменении кода — для разработки")
    run.add_argument("--no-browser", action="store_true", help="не открывать браузер после запуска")
    reset = sub.add_parser("reset-password", help="задать новый пароль пользователю")
    reset.add_argument("username")
    args = parser.parse_args(argv)

    config = Config.from_env()
    if args.data:
        config = replace(config, data_dir=args.data.expanduser().resolve())

    if args.command == "reset-password":
        return _reset_password(config, args.username)

    import os

    try:
        import uvicorn
    except ImportError:
        print("Не установлены зависимости. Удалите папку .venv и запустите start.sh (start.bat) снова.", file=sys.stderr)
        return 1

    host = getattr(args, "host", None) or config.host
    port = getattr(args, "port", None) or config.port
    os.environ["PERIMETER_DATA"] = str(config.data_dir)
    local = host in ("127.0.0.1", "localhost", "::1")
    url = f"http://{'127.0.0.1' if local else host}:{port}"
    print(f"Периметр: {url}  (данные: {config.data_dir})")
    if local and not getattr(args, "no_browser", False) and not os.environ.get("PERIMETER_NO_BROWSER"):
        _open_browser_when_ready(url)
    uvicorn.run(
        "perimeter.web.app:create_app",
        factory=True,
        host=host,
        port=port,
        reload=getattr(args, "reload", False),
        proxy_headers=True,
        log_level="info",
    )
    return 0


def _open_browser_when_ready(url: str) -> None:
    """Открывает панель в браузере, как только сервер начал отвечать."""
    import threading
    import time
    import urllib.request
    import webbrowser

    def wait_and_open() -> None:
        for _ in range(60):
            try:
                urllib.request.urlopen(f"{url}/health", timeout=1)
            except OSError:
                time.sleep(0.25)
                continue
            webbrowser.open(url)
            return

    threading.Thread(target=wait_and_open, daemon=True).start()


def _reset_password(config: Config, username: str) -> int:
    from sqlalchemy import select

    from .core.db import Database
    from .core.models import User
    from .core.security import hash_password, password_problem

    db = Database(config.db_url)
    with db.session() as s:
        user = s.scalar(select(User).where(User.username == username))
        if user is None:
            print(f"Пользователь {username!r} не найден", file=sys.stderr)
            return 1
        password = getpass.getpass("Новый пароль: ")
        if problem := password_problem(password):
            print(problem, file=sys.stderr)
            return 1
        if password != getpass.getpass("Ещё раз: "):
            print("Пароли не совпадают", file=sys.stderr)
            return 1
        user.password_hash = hash_password(password)
        user.session_epoch += 1
    print("Пароль изменён, все входы завершены")
    return 0


if __name__ == "__main__":
    sys.exit(main())
