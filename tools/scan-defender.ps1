# Прогоняет Microsoft Defender по готовым файлам выпуска и печатает результат
# в журнал задачи.
#
# ЗАЧЕМ. У людей на Windows ругаются Defender и SmartScreen, а точное имя
# срабатывания узнать неоткуда: у пользователя окно уже закрылось, а у нас
# Windows нет. На раннерах GitHub стоит НАСТОЯЩИЙ Defender с настоящими базами —
# значит имя угрозы (вида «Program:Win32/Wacapew.C!ml») можно добыть самим, в
# том же прогоне, что собрал файл.
#
# ПРЕДУПРЕЖДАЕМ, А НЕ РОНЯЕМ. Скрипт ВСЕГДА завершается нулём. Решение выносит
# обучаемая модель Microsoft (суффикс `!ml`), её базы меняются каждый день, и
# красный выпуск из-за чужой модели остановил бы выкладку файлов, с которыми всё
# в порядке. Сторож крт-статика — другое дело, он проверяет НАШУ ошибку и валит
# задачу; здесь же мы только светим фонарём.
#
# Вывод ТОЛЬКО латиницей: консоль Windows роняет шаг на кириллице.
param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Files)

$ErrorActionPreference = 'Continue'

# Рабочая копия MpCmdRun.exe живёт в ProgramData\...\Platform\<версия>, а в
# Program Files лежит заглушка постарше. Берём самую свежую платформу.
$mp = Get-ChildItem "$env:ProgramData\Microsoft\Windows Defender\Platform\*\MpCmdRun.exe" `
        -ErrorAction SilentlyContinue | Sort-Object FullName -Descending | Select-Object -First 1
if (-not $mp) {
    $mp = Get-Item "$env:ProgramFiles\Windows Defender\MpCmdRun.exe" -ErrorAction SilentlyContinue
}
if (-not $mp) {
    Write-Host "Defender: MpCmdRun.exe not found on this runner -- nothing to scan"
    exit 0
}
Write-Host "Defender engine: $($mp.FullName)"

try {
    Get-MpComputerStatus | Select-Object AMServiceEnabled, AntivirusEnabled, RealTimeProtectionEnabled,
        AMEngineVersion, AntivirusSignatureVersion, AntivirusSignatureLastUpdated | Format-List | Out-String | Write-Host
} catch {
    Write-Host "Defender: Get-MpComputerStatus unavailable ($($_.Exception.Message))"
}

# Без свежих баз результат ничего не значит: раннер может держать сборку
# двухнедельной давности, а имя угрозы у моделей меняется чаще.
Write-Host "--- signature update ---"
& $mp.FullName -SignatureUpdate 2>&1 | Out-String | Write-Host

foreach ($f in $Files) {
    $path = (Resolve-Path -LiteralPath $f -ErrorAction SilentlyContinue)
    if (-not $path) { Write-Host "SCAN: $f -- file not found, skipped"; continue }

    Write-Host "--- scan: $path ---"
    # -DisableRemediation ОБЯЗАТЕЛЕН: без него Defender утащит файл в карантин
    # прямо посреди прогона, и следующие шаги (сторож импортов, выкладка в
    # релиз) упадут на «файла нет» вместо внятного сообщения.
    & $mp.FullName -Scan -ScanType 3 -File $path -DisableRemediation 2>&1 | Out-String | Write-Host
    $code = $LASTEXITCODE
    # 0 — чисто, 2 — найдено (у MpCmdRun это же значение возвращается и при
    # отказе сканирования, поэтому смотрим ещё и на список угроз ниже).
    Write-Host "SCAN RESULT: $([System.IO.Path]::GetFileName($path)) -> exit code $code"
}

Write-Host "--- threats known to Defender after the scan ---"
try {
    $threats = Get-MpThreatDetection -ErrorAction SilentlyContinue
    if ($threats) {
        $threats | ForEach-Object {
            $name = (Get-MpThreat -ErrorAction SilentlyContinue |
                     Where-Object ThreatID -eq $_.ThreatID | Select-Object -First 1).ThreatName
            Write-Host "THREAT: $name | resources: $($_.Resources -join ', ')"
        }
    } else {
        Write-Host "THREAT: none reported"
    }
} catch {
    Write-Host "Defender: threat list unavailable ($($_.Exception.Message))"
}

# Всегда ноль — см. шапку.
exit 0
