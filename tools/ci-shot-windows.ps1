# Full-screen shots of the LIVE bmv-gui window on a Windows runner - one per tab.
#
# ASCII ONLY, ON PURPOSE. Everything this script prints ends up in the runner
# console, and Cyrillic there has already failed a job with an encoding error
# (see "Verify architecture" in release.yml). Comments are English for the same
# reason: a .ps1 without BOM is read as ANSI by Windows PowerShell.
#
# Steps: local coordinator + two real hosts (empty catalog would hide half of
# the app, see tools/ci-xff-relay.py), then launch the GUI, centre its window on
# the real desktop, and capture the WHOLE SCREEN once per tab - desktop, title
# bar and taskbar included. A cropped window rectangle looked like a mockup, not
# like a program running on somebody's machine.
#
# Tabs are switched with a REAL mouse click on the floating nav bar. Where to
# click and whether the right tab actually opened is decided by
# tools/ci-shot-check.py - the single source of the bar geometry, shared with
# the Linux job so the two cannot drift apart.
#
# TAB ORDER IS NOT ARBITRARY: VPN -> Host -> Server.
#   * VPN is open on launch, so the first frame needs no click at all.
#   * We only ever click cells of OTHER tabs. The cell of the current tab is not
#     navigation any more but a switch ("Start", "Share", see ui/app.slint), and
#     clicking it would start a connection instead of changing the page.
#   * Server goes last on purpose. Under the software rasteriser that tab kills
#     the process while the recent-servers list updates (a Slint bug, described
#     in restart_on_software_renderer). That update happens once, in the first
#     seconds after launch - that is, while we are shooting the first two tabs.
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
$check = 'tools\ci-shot-check.py'
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

# The checker reports by exit code; PowerShell does not stop on those by itself,
# so a green job with a failed check is exactly what happens if nobody looks.
function Invoke-Check([string[]]$argv) {
    $o = & python $check @argv 2>&1
    $o | ForEach-Object { Write-Host $_ }
    if ($LASTEXITCODE -ne 0) { throw "ci-shot-check.py failed: $($argv -join ' ')" }
    return $o
}

try {
    # 0. A bigger desktop. The runner boots at 1024x768, and the app takes its
    #    height from the screen (window_size in main.rs): 0.8*768-40 = 574 points,
    #    so the host list and the sharing form ran under the nav bar. 1080 gives
    #    824 points - the same window a person with an ordinary monitor sees.
    #    MUST happen BEFORE the GUI starts: the size is read once, at startup.
    #    Wrapped in try/catch on purpose - a runner image without the cmdlet
    #    should still produce shots, just smaller ones.
    try {
        Set-DisplayResolution -Width 1920 -Height 1080 -Force -ErrorAction Stop
        Write-Host 'desktop resolution set to 1920x1080'
    } catch {
        Write-Host "could not change resolution, keeping the default: $($_.Exception.Message)"
    }

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
    # NO SLINT_BACKEND HINT HERE, ON PURPOSE. This runner has no GPU and no
    # OpenGL at all - exactly the machine a user with a fresh Windows and no
    # display driver has. The app must cope on its own, and it does: the GPU
    # path fails at window creation, and the app restarts ITSELF with the
    # software rasteriser (restart_on_software_renderer in main.rs). Setting
    # SLINT_BACKEND here would test our hint instead of that mechanism.
    #
    # The loop guard is NOT this variable - it is the --software-renderer flag
    # the app passes to its own child. It was moved out of the environment on
    # purpose: an environment variable is inherited by the restart the UPDATER
    # makes, so a machine that once fell back would stay on the slow rasteriser
    # for good, even after its display driver was installed.
    #
    # Consequence for the code below: the window belongs to a DIFFERENT process
    # than the one we started. The first process exits with code 0 as soon as it
    # has spawned the second, so we look up the window by executable name.
    #
    # GetClientRect + ClientToScreen, not GetWindowRect: the click coordinates
    # are computed from the Slint layout, which knows nothing about the title
    # bar or the resize border. GetDpiForWindow turns those layout points into
    # pixels; the runner sits at 100%, but the arithmetic must not depend on it.
    Add-Type @'
using System;
using System.Runtime.InteropServices;
public class Win32 {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int ht, bool repaint);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, IntPtr e);
}
'@

    # Clear the desktop BEFORE the app opens. The runner keeps its own console
    # full of JSON on screen; it covered half of what was asked for - the desktop
    # around the window. Minimising first (rather than after) means our window
    # never gets minimised along with the rest.
    (New-Object -ComObject Shell.Application).MinimizeAll()
    Start-Sleep -Seconds 1

    $env:BEMYVPN_CONFIG = "$tmp\gui.toml"
    $started = Start-Bg $gui @() 'gui' $false
    $h = [IntPtr]::Zero
    $owner = 0
    for ($i = 0; $i -lt 60; $i++) {
        $live = @(Get-Process -Name 'bemyvpn-gui' -ErrorAction SilentlyContinue)
        foreach ($p in $live) {
            $p.Refresh()
            $cand = $p.MainWindowHandle
            if ($cand -eq [IntPtr]::Zero) { continue }
            # A HANDLE IS NOT A WINDOW YET. femtovg creates the winit window FIRST
            # and only then discovers that OpenGL is missing, so the dying process
            # owns a real handle for a moment - with a 0x0 rect, because the window
            # was never mapped. Grabbing that one produced a zero-sized bitmap and
            # failed the job. Size is the honest test of "a window a human sees".
            $probe = New-Object Win32+RECT
            [void][Win32]::GetWindowRect($cand, [ref]$probe)
            if (($probe.Right - $probe.Left) -lt 100 -or ($probe.Bottom - $probe.Top) -lt 100) { continue }
            $h = $cand; $owner = $p.Id; break
        }
        if ($h -ne [IntPtr]::Zero) { break }
        # No process left and no window: nobody is going to draw anything.
        if ($live.Count -eq 0) { throw 'GUI exited and never opened a window' }
        Start-Sleep -Seconds 1
    }
    if ($h -eq [IntPtr]::Zero) { throw 'no window appeared within 60 s' }
    # Say out loud WHICH path was exercised, so a future run cannot quietly stop
    # testing the fallback: a different pid means the GPU path failed and the app
    # restarted itself on the software rasteriser, which is what this runner has
    # to do. The same pid would mean it found working OpenGL.
    $how = if ($owner -eq $started.Id) { 'GPU path (OpenGL found)' } else { 'software rasteriser after self-restart' }
    Write-Host "window handle: $h, pid $owner (started $($started.Id)) - $how"

    Add-Type -AssemblyName System.Drawing
    Add-Type -AssemblyName System.Windows.Forms
    $sb = [Windows.Forms.Screen]::PrimaryScreen.Bounds
    Write-Host "desktop: $($sb.Width)x$($sb.Height)"

    [void][Win32]::ShowWindow($h, 5)   # SW_SHOW
    [void][Win32]::SetForegroundWindow($h)

    # Centre the window: wherever the app happened to land, a shot of the whole
    # desktop should show it sitting on that desktop, not glued to an edge.
    $wr = New-Object Win32+RECT
    [void][Win32]::GetWindowRect($h, [ref]$wr)
    $ww = $wr.Right - $wr.Left; $wh = $wr.Bottom - $wr.Top
    [void][Win32]::MoveWindow($h, [int](($sb.Width - $ww) / 2), [int](($sb.Height - $wh) / 2), $ww, $wh, $true)

    # The catalog reaches the window with the first snapshot (6 s budget in
    # bmv-signal); painting on a GPU-less runner takes its own time.
    Start-Sleep -Seconds 10

    $cr = New-Object Win32+RECT
    [void][Win32]::GetClientRect($h, [ref]$cr)
    $org = New-Object Win32+POINT
    [void][Win32]::ClientToScreen($h, [ref]$org)
    $dpi = [Win32]::GetDpiForWindow($h) / 96.0
    if ($dpi -le 0) { $dpi = 1.0 }
    # Scale printed culture-invariantly: a runner with a comma decimal separator
    # would hand Python "1,25" and it would refuse the number.
    $rect = @("$($org.X)", "$($org.Y)", "$($cr.Right)", "$($cr.Bottom)",
        $dpi.ToString([Globalization.CultureInfo]::InvariantCulture))
    Write-Host "client area: $($cr.Right)x$($cr.Bottom) at $($org.X),$($org.Y), scale $dpi"

    function Shoot([int]$tab, [string]$name) {
        # Park the cursor off the bar: it can land in the frame, and holding it
        # over a cell would mean measuring the hovered cell instead of the open one.
        [void][Win32]::SetCursorPos(4, 4)
        Start-Sleep -Seconds 3
        $path = Join-Path $out "windows-$name.png"
        $bmp = New-Object Drawing.Bitmap $sb.Width, $sb.Height
        $g = [Drawing.Graphics]::FromImage($bmp)
        $g.CopyFromScreen($sb.X, $sb.Y, 0, 0, $bmp.Size)
        $g.Dispose(); $bmp.Save($path, [Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
        Invoke-Check (@('check', $path) + $script:rect + @("$tab")) | Out-Null
    }

    function Open-Tab([int]$tab) {
        $xy = (Invoke-Check (@('coords') + $script:rect + @("$tab")) | Select-Object -Last 1).ToString().Split(' ')
        Write-Host "click on tab ${tab} at $($xy -join ',')"
        [void][Win32]::SetForegroundWindow($h)
        [void][Win32]::SetCursorPos([int]$xy[0], [int]$xy[1])
        Start-Sleep -Milliseconds 500
        [Win32]::mouse_event(0x02, 0, 0, 0, [IntPtr]::Zero)   # LEFTDOWN
        Start-Sleep -Milliseconds 120
        [Win32]::mouse_event(0x04, 0, 0, 0, [IntPtr]::Zero)   # LEFTUP
        Start-Sleep -Seconds 2
    }

    Shoot 0 'vpn'
    Open-Tab 1
    Shoot 1 'host'
    Open-Tab 2
    Shoot 2 'server'

    Invoke-Check (@('distinct',
        (Join-Path $out 'windows-vpn.png'),
        (Join-Path $out 'windows-host.png'),
        (Join-Path $out 'windows-server.png')) + $rect) | Out-Null
}
finally {
    foreach ($p in $procs) { if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue } }
    # By name as well: the window belongs to the process the app restarted itself
    # as, and that one was never in $procs.
    Stop-Process -Name 'bemyvpn-gui' -Force -ErrorAction SilentlyContinue
    # Logs travel with the artifact: a red run is unreadable without them.
    Get-ChildItem -Path $tmp -Filter '*.out' -ErrorAction SilentlyContinue | Copy-Item -Destination $out
    Get-ChildItem -Path $tmp -Filter '*.err' -ErrorAction SilentlyContinue | Copy-Item -Destination $out
}
