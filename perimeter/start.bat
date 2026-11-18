@echo off
rem Запуск панели на Windows. Первый запуск сам ставит всё нужное.
chcp 65001 >nul
cd /d "%~dp0"

if not exist ".venv\Scripts\python.exe" (
  echo Первый запуск: готовлю окружение ^(это один раз^)...
  py -3 -m venv .venv || python -m venv .venv
  ".venv\Scripts\python.exe" -m pip install --quiet --upgrade pip
  ".venv\Scripts\python.exe" -m pip install --quiet -e .
)

".venv\Scripts\python.exe" -m perimeter run %*
