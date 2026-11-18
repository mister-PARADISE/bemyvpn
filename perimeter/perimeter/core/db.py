"""База данных: SQLite через SQLAlchemy 2.0.

Таблицы объявляют сами расширения (наследуют `Base`), ядро только создаёт их
и прогоняет миграции. Миграция — функция, привязанная к номеру версии:

    ctx.db.migration("clients", 2, add_column_x)

Номер версии хранится отдельно для каждого расширения, поэтому расширения
развиваются независимо друг от друга.
"""

from __future__ import annotations

import logging
from contextlib import contextmanager
from datetime import datetime, timezone
from typing import Callable, Iterator

from sqlalchemy import DateTime, Integer, String, create_engine, event, select
from sqlalchemy.engine import Connection, Engine
from sqlalchemy.orm import DeclarativeBase, Mapped, Session, mapped_column, sessionmaker

log = logging.getLogger("perimeter.db")


def utcnow() -> datetime:
    """Текущее время в UTC без tzinfo — так его хранит SQLite."""
    return datetime.now(timezone.utc).replace(tzinfo=None)


class Base(DeclarativeBase):
    pass


class TimestampMixin:
    created_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow, nullable=False)
    updated_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow, onupdate=utcnow, nullable=False)


class SchemaVersion(Base):
    __tablename__ = "schema_version"
    component: Mapped[str] = mapped_column(String(64), primary_key=True)
    version: Mapped[int] = mapped_column(Integer, nullable=False)


Migration = Callable[[Connection], None]


class Database:
    def __init__(self, url: str) -> None:
        self.url = url
        self.engine: Engine = create_engine(url, connect_args={"check_same_thread": False})
        if url.startswith("sqlite"):
            event.listen(self.engine, "connect", _sqlite_pragmas)
        self._sessions = sessionmaker(self.engine, expire_on_commit=False)
        self._migrations: dict[str, dict[int, Migration]] = {}

    @contextmanager
    def session(self) -> Iterator[Session]:
        """Сессия с автоматическим commit/rollback."""
        s = self._sessions()
        try:
            yield s
            s.commit()
        except Exception:
            s.rollback()
            raise
        finally:
            s.close()

    def new_session(self) -> Session:
        return self._sessions()

    def migration(self, component: str, version: int, fn: Migration) -> None:
        if version < 2:
            raise ValueError("Версия 1 — это create_all; миграции начинаются со 2")
        self._migrations.setdefault(component, {})[version] = fn

    def migrate(self, components: list[str]) -> None:
        """Создаёт недостающие таблицы и догоняет версии схемы.

        Новая установка сразу получает последнюю схему через create_all, поэтому
        её версия выставляется в максимальную без прогона миграций.
        """
        with self.engine.begin() as conn:
            known = {row.component: row.version for row in conn.execute(select(SchemaVersion))} \
                if self.engine.dialect.has_table(conn, SchemaVersion.__tablename__) else {}
            Base.metadata.create_all(conn)
            for component in components:
                steps = self._migrations.get(component, {})
                latest = max(steps, default=1)
                current = known.get(component)
                if current is None:
                    conn.execute(SchemaVersion.__table__.insert().values(component=component, version=latest))
                    continue
                for version in sorted(v for v in steps if v > current):
                    log.info("Миграция %s → %s", component, version)
                    steps[version](conn)
                if latest > current:
                    conn.execute(
                        SchemaVersion.__table__.update()
                        .where(SchemaVersion.component == component)
                        .values(version=latest)
                    )


def _unicode_lower(value):
    return value.lower() if isinstance(value, str) else value


def _sqlite_pragmas(dbapi_conn, _record) -> None:
    # Встроенный lower() в SQLite понимает только латиницу: «Ромашка» не нашлась бы по «ромаш».
    # ilike в SQLAlchemy строится как lower(x) LIKE lower(y), поэтому подменяем lower на питоновский.
    dbapi_conn.create_function("lower", 1, _unicode_lower, deterministic=True)
    cur = dbapi_conn.cursor()
    cur.execute("PRAGMA foreign_keys=ON")
    cur.execute("PRAGMA journal_mode=WAL")
    cur.execute("PRAGMA busy_timeout=5000")
    cur.close()
