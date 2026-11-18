"""Старая база (версия 1 с «разрешениями») догоняет схему по шагам и продолжает работать."""

from __future__ import annotations

import sqlite3

from sqlalchemy import select

from perimeter.extensions.targets.models import Target

from .conftest import make_app

OLD_TARGET = """
CREATE TABLE target (
  id INTEGER NOT NULL PRIMARY KEY,
  domain VARCHAR(253) NOT NULL UNIQUE,
  title VARCHAR(160) NOT NULL,
  include JSON NOT NULL,
  exclude JSON NOT NULL,
  notes TEXT NOT NULL,
  auth_status VARCHAR(16) NOT NULL,
  auth_method VARCHAR(16),
  auth_token VARCHAR(64) NOT NULL,
  auth_note TEXT NOT NULL,
  auth_verified_at DATETIME,
  auth_valid_until DATE,
  created_at DATETIME NOT NULL,
  updated_at DATETIME NOT NULL
);
CREATE INDEX ix_target_auth_status ON target (auth_status);
CREATE TABLE schema_version (component VARCHAR(64) NOT NULL PRIMARY KEY, version INTEGER NOT NULL);
INSERT INTO schema_version VALUES ('core', 1), ('targets', 1);
INSERT INTO target VALUES (1, 'example.com', 'Магазин', '[]', '[]', '', 'verified', 'document', 'aa',
  'Договор №1', '2026-11-18 10:00:00', NULL, '2026-11-18 10:00:00', '2026-11-18 10:00:00');
INSERT INTO target VALUES (2, 'example.org', '', '[]', '[]', '', 'pending', NULL, 'bb', '', NULL, NULL,
  '2026-11-18 10:00:00', '2026-11-18 10:00:00');
"""


def test_v1_database_is_migrated(tmp_path):
    data = tmp_path / "data"
    data.mkdir()
    with sqlite3.connect(data / "perimeter.db") as conn:
        conn.executescript(OLD_TARGET)
    ctx = make_app(tmp_path).state.ctx
    with ctx.db.session() as s:
        old = {t.domain: t for t in s.scalars(select(Target))}
        assert set(old) == {"example.com", "example.org"} and old["example.com"].title == "Магазин"
        s.add(Target(domain="example.net"))  # новые записи вставляются без старых колонок
    with sqlite3.connect(data / "perimeter.db") as conn:
        cols = {row[1] for row in conn.execute("PRAGMA table_info(target)")}
        version = conn.execute("SELECT version FROM schema_version WHERE component='targets'").fetchone()[0]
    assert not any(c.startswith(("auth_", "consent")) for c in cols)
    assert version == 3
    # повторный запуск ничего не ломает
    make_app(tmp_path)


def test_new_database_starts_at_latest_version(tmp_path):
    make_app(tmp_path)
    with sqlite3.connect(tmp_path / "data" / "perimeter.db") as conn:
        version = conn.execute("SELECT version FROM schema_version WHERE component='targets'").fetchone()[0]
    assert version == 3
