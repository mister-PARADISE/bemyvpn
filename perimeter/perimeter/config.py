"""Конфигурация запуска: где лежат данные, на каком адресе слушать.

Всё, что меняется из панели, живёт в настройках (core.settings), а здесь —
только то, что нужно знать до старта приложения.
"""

from __future__ import annotations

import os
import secrets
from dataclasses import dataclass, field, replace
from pathlib import Path


@dataclass(frozen=True)
class Config:
    data_dir: Path
    host: str = "127.0.0.1"
    port: int = 8710
    debug: bool = False
    # Модули Python, которые грузятся как встроенные расширения (по порядку).
    builtin_extensions: tuple[str, ...] = (
        "perimeter.extensions.dashboard",
        "perimeter.extensions.targets",
        "perimeter.extensions.catalog",
        "perimeter.extensions.journal",
        "perimeter.extensions.settings_page",
    )
    extra_extensions: tuple[str, ...] = field(default=())

    @classmethod
    def from_env(cls, **overrides) -> "Config":
        env = os.environ
        data_dir = Path(env.get("PERIMETER_DATA", Path.cwd() / "data"))
        cfg = cls(
            data_dir=data_dir.expanduser().resolve(),
            host=env.get("PERIMETER_HOST", "127.0.0.1"),
            port=int(env.get("PERIMETER_PORT", "8710")),
            debug=env.get("PERIMETER_DEBUG", "") in {"1", "true", "yes"},
        )
        return replace(cfg, **overrides) if overrides else cfg

    @property
    def db_url(self) -> str:
        return f"sqlite:///{self.data_dir / 'perimeter.db'}"

    @property
    def plugins_dir(self) -> Path:
        """Сюда кладутся собственные расширения: папка с __init__.py и setup(ctx)."""
        return self.data_dir / "plugins"

    def ensure_dirs(self) -> None:
        self.data_dir.mkdir(parents=True, exist_ok=True)
        self.plugins_dir.mkdir(exist_ok=True)

    def secret_key(self) -> str:
        """Ключ подписи сессий. Создаётся один раз и хранится рядом с базой."""
        self.ensure_dirs()
        path = self.data_dir / "secret.key"
        if not path.exists():
            path.write_text(secrets.token_urlsafe(48), encoding="utf-8")
            try:
                path.chmod(0o600)
            except OSError:  # Windows и экзотические ФС
                pass
        return path.read_text(encoding="utf-8").strip()
