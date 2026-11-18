#!/usr/bin/env bash
# Запуск панели на macOS и Linux. Первый запуск сам ставит всё нужное.
set -euo pipefail
cd "$(dirname "$0")"

if [ ! -x .venv/bin/python ]; then
  echo "Первый запуск: готовлю окружение (это один раз)…"
  python3 -m venv .venv
  .venv/bin/pip install --quiet --upgrade pip
  .venv/bin/pip install --quiet -e .
fi

exec .venv/bin/python -m perimeter run "$@"
