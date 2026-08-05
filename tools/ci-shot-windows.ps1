# Screenshot of the LIVE bmv-gui window on a Windows runner.
#
# ASCII ONLY, ON PURPOSE. Everything this script prints ends up in the runner
# console, and Cyrillic there has already failed a job with an encoding error
# (see "Verify architecture" in release.yml). Comments are English for the same
# reason: a .ps1 without BOM is read as ANSI by Windows PowerShell.
#
# Steps: local coordinator + two real hosts (empty catalog would hide half of
# the app, see tools/ci-xff-relay.py), then launch the GUI, wait for its window,
# capture it with plain GDI (System.Drawing.CopyFromScreen) and refuse to
# publish a frame that is not actually painted.
#
# The exe carries a UAC requireAdministrator manifest (apps/bmv-gui/build.rs),
# so it only starts when the parent process is already elevated. The GitHub
# runner is. Nothing is connected here, so no password is ever asked.
$ErrorActionPreference = 'Stop'

$out = if ($args.Count -ge 1) { $args[0] } else { 'shot' }
New-Item -ItemType Directory -Force -Path $out | Out-Null
$tmp = Join-Path $env:RUNNER_TEMP 'bmvshot'
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

$gui = 'target\release\bemyvpn-gui.exe'
$cli = 'target\release\bemyvpn.exe'
$procs = @()

# $hidden: console helpers start with SW_HIDE so their windows never end up in
# the frame. The GUI must NOT be started that way - the first ShowWindow call of
# a process obeys the startup info, so SW_HIDE would keep the app window hidden.
function Start-Bg([string]$exe, [string[]]$argv, [string]$log, [bool]$hidden = $true) {
    $common = @{
        FilePath                = $exe
        PassThru                = $true
        RedirectStandardOutput  = (Join-Path $tmp "$log.out")
        RedirectStandardError   = (Join-Path $tmp "$log.err")
    }
    if ($argv.Count -gt 0) { $common['ArgumentList'] = $argv }
    if ($hidden) { $common['WindowStyle'] = 'Hidden' }
    $p = Start-Process @common
    $script:procs += $p
    return $p
}

try {
    # 1. Local coordinator, address spoofing relay, two hosts.
    "[host]`npublic = true`nmax_guests = 8" | Set-Content -Path "$tmp\host.toml" -Encoding utf8
    'coordinators = ["http://127.0.0.1:3330"]' | Set-Content -Path "$tmp\gui.toml" -Encoding utf8

    Start-Bg $cli @('server', '--bind', '127.0.0.1:3330') 'coord' | Out-Null
    Start-Bg 'python' @('tools\ci-xff-relay.py', '3330', '3331:8.8.8.8', '3332:77.88.55.88') 'relay' | Out-Null
    Start-Sleep -Seconds 2

    # Host names are Cyrillic on purpose: rendering them is part of what we check.
    # No spaces inside a name - Start-Process glues ArgumentList entries together
    # without quoting, so "Host-1 (CI)" arrived as two arguments and the CLI
    # refused it. Same names as the Linux job, or the two shots do not compare.
    $host1 = "$([char]0x0425)$([char]0x043E)$([char]0x0441)$([char]0x0442)-1(CI)"
    $host2 = "$([char]0x0425)$([char]0x043E)$([char]0x0441)$([char]0x0442)-2(CI)"
    Start-Bg $cli @('--config', "$tmp\host.toml", '--coordinator', 'http://127.0.0.1:3331',
        'host', '--name', $host1) 'host1' | Out-Null
    Start-Bg $cli @('--config', "$tmp\host.toml", '--coordinator', 'http://127.0.0.1:3332',
        'host', '--name', $host2) 'host2' | Out-Null

    # 2. The catalog must not be empty before we bother with the window.
    $seen = 0
    for ($i = 0; $i -lt 20; $i++) {
        $p = Start-Process -FilePath $cli -ArgumentList @('--coordinator', 'http://127.0.0.1:3330', 'guest') `
            -PassThru -Wait -WindowStyle Hidden `
            -RedirectStandardOutput "$tmp\dir.out" -RedirectStandardError "$tmp\dir.err"
        $txt = [IO.File]::ReadAllText("$tmp\dir.out", [Text.Encoding]::UTF8)
        $seen = ([regex]::Matches($txt, '\(CI\)')).Count
        if ($seen -ge 2) { break }
        Start-Sleep -Seconds 1
    }
    Write-Host "hosts in catalog: $seen"
    if ($seen -lt 2) {
        $h1 = [IO.File]::ReadAllText("$tmp\host1.out", [Text.Encoding]::UTF8)
        Write-Host ('host1 log: ' + ($h1 -replace '[^\x20-\x7E\r\n]', '.'))
        throw 'hosts never reached the catalog, nothing worth capturing'
    }

    # 3. Window.
    #
    # SOFTWARE RENDERER, ON PURPOSE. The runner has no GPU and no OpenGL at all:
    # the default femtovg renderer dies at startup with "Failed to initialize
    # OpenGL driver: Could not locate glCreateShader symbol" and the process
    # exits with code 1. Slint ships a software rasterizer in its default
    # features, so winit-software needs no change to Cargo.toml.
    #
    # Price of this, stated plainly: the frame below is drawn by Slint's own
    # rasteriser, not by the GPU path a real user gets. Layout, text, fonts,
    # flags and colours are the real thing; antialiasing and gradients may
    # differ by a hair from a machine with a video card. The Linux job keeps
    # femtovg (llvmpipe), so the GPU path is still covered there.
    $env:SLINT_BACKEND = 'winit-software'
    $env:BEMYVPN_CONFIG = "$tmp\gui.toml"
    $app = Start-Bg $gui @() 'gui' $false
    $h = [IntPtr]::Zero
    for ($i = 0; $i -lt 60; $i++) {
        if ($app.HasExited) { throw "GUI exited early, code $($app.ExitCode)" }
        $app.Refresh()
        if ($app.MainWindowHandle -ne [IntPtr]::Zero) { $h = $app.MainWindowHandle; break }
        Start-Sleep -Seconds 1
    }
    if ($h -eq [IntPtr]::Zero) { throw 'no window appeared within 60 s' }
    Write-Host "window handle: $h"

    Add-Type @'
using System;
using System.Runtime.InteropServices;
public class Win32 {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int attr, out RECT r, int size);
}
'@
    [void][Win32]::ShowWindow($h, 5)   # SW_SHOW
    [void][Win32]::SetForegroundWindow($h)
    # The catalog reaches the window with the first snapshot (6 s budget in
    # bmv-signal); painting on a GPU-less runner takes its own time.
    Start-Sleep -Seconds 10

    Add-Type -AssemblyName System.Drawing
    Add-Type -AssemblyName System.Windows.Forms

    # DWMWA_EXTENDED_FRAME_BOUNDS (9), not GetWindowRect. GetWindowRect returns
    # the rect INCLUDING the invisible DWM resize border, so the capture picked
    # up strips of whatever sat behind the window - and those foreign pixels fed
    # the "is the frame painted" check with colours the app never drew.
    $r = New-Object Win32+RECT
    if ([Win32]::DwmGetWindowAttribute($h, 9, [ref]$r, 16) -ne 0) {
        [void][Win32]::GetWindowRect($h, [ref]$r)
        Write-Host 'dwm frame bounds unavailable, falling back to GetWindowRect'
    }
    Write-Host "window rect: $($r.Left),$($r.Top) $($r.Right - $r.Left)x$($r.Bottom - $r.Top)"

    function Grab([int]$x, [int]$y, [int]$w, [int]$ht, [string]$path) {
        $bmp = New-Object Drawing.Bitmap $w, $ht
        $g = [Drawing.Graphics]::FromImage($bmp)
        $g.CopyFromScreen($x, $y, 0, 0, $bmp.Size)
        $g.Dispose()
        $bmp.Save($path, [Drawing.Imaging.ImageFormat]::Png)
        return $bmp
    }

    $win = Grab $r.Left $r.Top ($r.Right - $r.Left) ($r.Bottom - $r.Top) (Join-Path $out 'windows-okno.png')
    $sb = [Windows.Forms.Screen]::PrimaryScreen.Bounds
    Write-Host "desktop: $($sb.Width)x$($sb.Height)"
    (Grab $sb.X $sb.Y $sb.Width $sb.Height (Join-Path $out 'windows-ekran.png')).Dispose()

    # 4. A grey rectangle is not a check. Count distinct colours on a grid.
    $seenColors = New-Object 'System.Collections.Generic.HashSet[int]'
    for ($y = 0; $y -lt $win.Height; $y += 3) {
        for ($x = 0; $x -lt $win.Width; $x += 3) {
            [void]$seenColors.Add($win.GetPixel($x, $y).ToArgb())
        }
    }
    $n = $seenColors.Count
    $win.Dispose()
    Write-Host "window frame: $($sb.Width)x$($sb.Height) desktop, distinct colours in window: $n"
    if ($n -lt 200) { throw "only $n distinct colours - the window did not paint" }
}
finally {
    foreach ($p in $procs) { if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue } }
    # Logs travel with the artifact: a red run is unreadable without them.
    Get-ChildItem -Path $tmp -Filter '*.out' -ErrorAction SilentlyContinue | Copy-Item -Destination $out
    Get-ChildItem -Path $tmp -Filter '*.err' -ErrorAction SilentlyContinue | Copy-Item -Destination $out
}
