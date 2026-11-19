#!/usr/bin/env bash
# Запуск панели на macOS и Linux. Первый запуск сам ставит всё нужное.
# Окружение, собранное старым Python или не до конца (оборвалась установка), пересобирается само.
set -euo pipefail
cd "$(dirname "$0")"

fits() { "$1" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)' >/dev/null 2>&1; }

if [ -e .venv ] && ! { [ -x .venv/bin/python ] && fits .venv/bin/python; }; then
  echo "Окружение собрано старым Python — пересобираю…"
  rm -rf .venv
fi

if [ ! -x .venv/bin/python ]; then
  PY=""
  for candidate in "${PYTHON:-}" python3.13 python3.12 python3.11 python3 python; do
    if [ -n "$candidate" ] && command -v "$candidate" >/dev/null 2>&1 && fits "$candidate"; then
      PY="$candidate"
      break
    fi
  done
  if [ -z "$PY" ]; then
    echo "Нужен Python 3.11 или новее, а найден: $(python3 --version 2>&1 || echo 'никакой')."
    echo "  macOS:  brew install python@3.13   (или установщик с python.org)"
    echo "  pyenv:  pyenv install 3.13 && pyenv local 3.13"
    echo "Или укажите свой: PYTHON=/путь/к/python3.13 ./start.sh"
    exit 1
  fi
  echo "Первый запуск: готовлю окружение на $("$PY" --version 2>&1) (это один раз)…"
  "$PY" -m venv .venv
fi

# зависимости: ставим, если ещё не стояли или pyproject.toml новее прошлой установки
if [ ! -f .venv/.ready ] || [ pyproject.toml -nt .venv/.ready ]; then
  echo "Ставлю зависимости…"
  .venv/bin/python -m pip install --quiet --upgrade pip
  # только зависимости из pyproject.toml: сама панель запускается прямо из этой папки
  .venv/bin/python -c 'import tomllib; print("\n".join(tomllib.load(open("pyproject.toml", "rb"))["project"]["dependencies"]))' > .venv/requirements.txt
  .venv/bin/python -m pip install --quiet -r .venv/requirements.txt
  touch .venv/.ready
fi

exec .venv/bin/python -m perimeter run "$@"
