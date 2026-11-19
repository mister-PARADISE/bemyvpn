@echo off
rem Запуск панели на Windows. Первый запуск сам ставит всё нужное.
rem Окружение, собранное старым Python или не до конца, пересобирается само.
chcp 65001 >nul
cd /d "%~dp0"
setlocal EnableDelayedExpansion

set "VPY=.venv\Scripts\python.exe"
set "FITS=import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)"

if exist ".venv" (
  set "GOOD="
  if exist "%VPY%" "%VPY%" -c "%FITS%" >nul 2>&1 && set "GOOD=1"
  if not defined GOOD (
    echo Окружение собрано старым Python - пересобираю...
    rmdir /s /q ".venv"
  )
)

if not exist "%VPY%" (
  set "PY="
  for %%P in ("py -3.13" "py -3.12" "py -3.11" "py -3" "python") do (
    if not defined PY (
      %%~P -c "%FITS%" >nul 2>&1 && set "PY=%%~P"
    )
  )
  if not defined PY (
    echo Нужен Python 3.11 или новее: https://www.python.org/downloads/
    echo При установке отметьте "Add python.exe to PATH", затем запустите start.bat снова.
    pause
    exit /b 1
  )
  echo Первый запуск: готовлю окружение - это один раз...
  !PY! -m venv .venv
  if errorlevel 1 (
    echo Не получилось создать окружение - см. ошибку выше.
    pause
    exit /b 1
  )
)

if not exist ".venv\.ready" (
  echo Ставлю зависимости...
  "%VPY%" -m pip install --quiet --upgrade pip
  rem только зависимости из pyproject.toml: сама панель запускается прямо из этой папки
  "%VPY%" -c "import tomllib; print(chr(10).join(tomllib.load(open('pyproject.toml', 'rb'))['project']['dependencies']))" > ".venv\requirements.txt"
  "%VPY%" -m pip install --quiet -r ".venv\requirements.txt"
  if errorlevel 1 (
    echo Зависимости не встали - см. ошибку выше.
    pause
    exit /b 1
  )
  type nul > ".venv\.ready"
)

"%VPY%" -m perimeter run %*
