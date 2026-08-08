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
# Второй аргумент 'nostrict' — «кадры снимай и выкладывай, но за них не отвечай».
# Нужен ровно для одного раннера: на windows-11-arm экран занят мастером первичной
# настройки Windows. Его окна нет в EnumWindows (пробовали: четыре чужих окна
# спрятались, мастер остался), HWND_TOPMOST его не перебивает, MinimizeAll не
# берёт — все кадры выходят снимками мастера. ЗАМЕРЫ (MEASURE и SCALE) от этого
# не страдают и остаются обязательными на обеих машинах: они читают размеры
# нашего окна, а не картинку с экрана.
$strict = -not ($args.Count -ge 2 -and $args[1] -eq 'nostrict')
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
    # The GUI goes through the relay too, not straight to 3330. Announcing from
    # 127.0.0.1 is refused by the coordinator (private address, 422), so a GUI
    # pointed at the bare port could never share at all - and the "start sharing"
    # shot below would show a failure that belongs to the test rig, not to the app.
    'coordinators = ["http://127.0.0.1:3331"]' | Set-Content -Path "$tmp\gui.toml" -Encoding utf8

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
    // DWMWA_EXTENDED_FRAME_BOUNDS (9): what a person actually SEES. GetWindowRect
    // on Windows 10/11 adds the INVISIBLE resize border (~7 px a side), so it
    // overstates the width of the window by ~15 px and would make any measurement
    // of "how wide is our window" wrong by that much.
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int attr, out RECT r, int size);
    // Our window must sit ABOVE everything. On the ARM runner the Windows
    // out-of-box wizard ("Choose privacy settings") owns the whole screen and is
    // topmost: MinimizeAll does not touch it, SetForegroundWindow does not beat
    // it, and every shot came out as a picture of that wizard. HWND_TOPMOST does.
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int ht, uint flags);
    // Sweeping foreign full-screen windows off the desktop. The ARM runner boots
    // straight into the Windows out-of-box wizard ("Choose privacy settings"),
    // which is topmost, has no minimise box, and survives both MinimizeAll and
    // HWND_TOPMOST on our own window - every frame came out a picture of it.
    // SW_HIDE takes any window, wizard or not.
    public delegate bool EnumProc(IntPtr h, IntPtr p);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr p);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
}
'@

    # Clear the desktop BEFORE the app opens. The runner keeps its own console
    # full of JSON on screen; it covered half of what was asked for - the desktop
    # around the window. Minimising first (rather than after) means our window
    # never gets minimised along with the rest.
    (New-Object -ComObject Shell.Application).MinimizeAll()
    Start-Sleep -Seconds 1

    # Wait for a window a human could see. A HANDLE IS NOT A WINDOW YET: femtovg
    # creates the winit window FIRST and only then discovers that OpenGL is
    # missing, so the dying process owns a real handle for a moment - with a 0x0
    # rect, because the window was never mapped. Grabbing that one produced a
    # zero-sized bitmap and failed the job. Size is the honest test.
    # Returns the handle plus the pid that owns it (not always the pid we started).
    function Wait-Window([int]$startedId) {
        for ($i = 0; $i -lt 60; $i++) {
            $live = @(Get-Process -Name 'bemyvpn-gui' -ErrorAction SilentlyContinue)
            foreach ($p in $live) {
                $p.Refresh()
                $cand = $p.MainWindowHandle
                if ($cand -eq [IntPtr]::Zero) { continue }
                $probe = New-Object Win32+RECT
                [void][Win32]::GetWindowRect($cand, [ref]$probe)
                if (($probe.Right - $probe.Left) -lt 100 -or ($probe.Bottom - $probe.Top) -lt 100) { continue }
                # Say out loud WHICH path was exercised, so a future run cannot
                # quietly stop testing the fallback: a different pid means the GPU
                # path failed and the app restarted itself on the software
                # rasteriser, which is what this runner has to do.
                $how = if ($p.Id -eq $startedId) { 'GPU path (OpenGL found)' } else { 'software rasteriser after self-restart' }
                Write-Host "window handle: $cand, pid $($p.Id) (started $startedId) - $how"
                return @($cand, $p.Id)
            }
            # No process left and no window: nobody is going to draw anything.
            if ($live.Count -eq 0) { throw 'GUI exited and never opened a window' }
            Start-Sleep -Seconds 1
        }
        throw 'no window appeared within 60 s'
    }

    $env:BEMYVPN_CONFIG = "$tmp\gui.toml"
    $started = Start-Bg $gui @() 'gui' $false
    $found = Wait-Window $started.Id
    $h = $found[0]; $owner = $found[1]

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
    # HWND_TOPMOST (-1) вместе с переносом: SWP_SHOWWINDOW = 0x0040.
    [void][Win32]::SetWindowPos($h, [IntPtr](-1), [int](($sb.Width - $ww) / 2),
        [int](($sb.Height - $wh) / 2), $ww, $wh, 0x0040)

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

    # HOW WIDE IS THIS WINDOW, REALLY. The complaint "too wide on Windows" has to
    # be answered by a number, not by a feeling, and by a number from THIS machine:
    # the layout asks for 400 logical points, but only the client area is ours -
    # the title bar and the border belong to the OS, and the invisible resize
    # border is another ~7 px a side that GetWindowRect counts and the eye does not.
    # All three are printed side by side, in pixels and in logical points.
    $fr = New-Object Win32+RECT
    [void][Win32]::DwmGetWindowAttribute($h, 9, [ref]$fr, 16)
    [void][Win32]::GetWindowRect($h, [ref]$wr)
    $inv = [Globalization.CultureInfo]::InvariantCulture
    # '0.##', not '0.0': the scale 1.25 printed as "1.3" and the table read as if
    # 125% had never been measured.
    function Fmt([double]$v) { return $v.ToString('0.##', $inv) }
    Write-Host ("MEASURE client {0}x{1} px = {2}x{3} pt" -f $cr.Right, $cr.Bottom,
        (Fmt ($cr.Right / $dpi)), (Fmt ($cr.Bottom / $dpi)))
    Write-Host ("MEASURE visible frame {0}x{1} px = {2}x{3} pt (border {4} pt a side, title {5} pt)" -f
        ($fr.Right - $fr.Left), ($fr.Bottom - $fr.Top),
        (Fmt (($fr.Right - $fr.Left) / $dpi)), (Fmt (($fr.Bottom - $fr.Top) / $dpi)),
        (Fmt ((($fr.Right - $fr.Left) - $cr.Right) / 2 / $dpi)),
        (Fmt ((($fr.Bottom - $fr.Top) - $cr.Bottom) / $dpi)))
    Write-Host ("MEASURE GetWindowRect {0}x{1} px (counts the invisible resize border)" -f
        ($wr.Right - $wr.Left), ($wr.Bottom - $wr.Top))
    Write-Host ("MEASURE desktop {0}x{1} px = {2}x{3} pt at scale {4}" -f $sb.Width, $sb.Height,
        (Fmt ($sb.Width / $dpi)), (Fmt ($sb.Height / $dpi)), (Fmt $dpi))

    # $judge = $false: снять кадр и НЕ судить его. Нужно ровно для «раздача
    # включена»: проверка узнаёт открытую вкладку по мятой заливке ячейки, а у
    # работающей раздачи ячейка становится «Стоп» и красится КРАСНЫМ — то есть
    # верный кадр она объявила бы поломкой.
    # Hide every visible top-level window of ANOTHER process that covers most of
    # the screen. Ours is spared by pid, so the sweep cannot hide the app itself.
    function Clear-Desktop([int]$keepPid) {
        $sw = [Windows.Forms.Screen]::PrimaryScreen.Bounds
        $cb = [Win32+EnumProc] {
            param($wh, $lp)
            if (-not [Win32]::IsWindowVisible($wh)) { return $true }
            $pid2 = 0
            [void][Win32]::GetWindowThreadProcessId($wh, [ref]$pid2)
            if ($pid2 -eq $keepPid) { return $true }
            $r = New-Object Win32+RECT
            [void][Win32]::GetWindowRect($wh, [ref]$r)
            if (($r.Right - $r.Left) -ge $sw.Width * 0.6 -and ($r.Bottom - $r.Top) -ge $sw.Height * 0.6) {
                Write-Host "hiding a full-screen window of pid $pid2"
                [void][Win32]::ShowWindow($wh, 0)   # SW_HIDE
            }
            return $true
        }
        [void][Win32]::EnumWindows($cb, [IntPtr]::Zero)
    }

    function Shoot([int]$tab, [string]$name, [bool]$judge = $true) {
        Clear-Desktop $script:owner
        [void][Win32]::SetForegroundWindow($script:h)
        # Park the cursor off the bar: it can land in the frame, and holding it
        # over a cell would mean measuring the hovered cell instead of the open one.
        [void][Win32]::SetCursorPos(4, 4)
        Start-Sleep -Seconds 3
        $path = Join-Path $out "windows-$name.png"
        $bmp = New-Object Drawing.Bitmap $sb.Width, $sb.Height
        $g = [Drawing.Graphics]::FromImage($bmp)
        $g.CopyFromScreen($sb.X, $sb.Y, 0, 0, $bmp.Size)
        $g.Dispose(); $bmp.Save($path, [Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
        if ($judge) { Invoke-Check (@('check', $path) + $script:rect + @("$tab")) | Out-Null }
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

    # The shots and the width sweep are INDEPENDENT answers to two different
    # questions, so a failure of the first must not swallow the second: the shots
    # depend on a desktop we do not own (see Clear-Desktop), the measurement does
    # not. The error is kept and rethrown after the sweep has printed its table.
    $shotError = $null
    try {

    Shoot 0 'vpn'
    Open-Tab 1
    Shoot 1 'host'

    # ACTUALLY START SHARING, from the window, the way a person does it.
    # Everything up to here only proves the app draws. The complaint was that
    # sharing does not work on Windows-ARM, and sharing is the one thing no shot
    # ever exercised: the cell of the OPEN tab is not navigation any more but the
    # switch itself ("Share"), so clicking it a second time is the button.
    #
    # No automatic verdict on purpose: whether the panel says "Sharing" with a
    # code or shows a red error is read off the picture. An assertion here would
    # have to know why a runner behind NAT failed, and it does not.
    Open-Tab 1
    Start-Sleep -Seconds 6
    Shoot 1 'host-sharing' $false

    Open-Tab 2
    Shoot 2 'server'

    Invoke-Check (@('distinct',
        (Join-Path $out 'windows-vpn.png'),
        (Join-Path $out 'windows-host.png'),
        (Join-Path $out 'windows-server.png')) + $rect) | Out-Null

    } catch { $shotError = $_; Write-Host "shots failed, measuring anyway: $($_.Exception.Message)" }

    # ── THE SAME WINDOW AT FOUR SCREEN SCALES ────────────────────────────────
    #
    # "Too wide on Windows" has to be answered for the scales people actually
    # run, not just for the runner's 100%. The width of the layout is 400 POINTS;
    # in pixels it must grow with the scale (600 at 150%), and in points it must
    # stay 400 at every one of them. Anything else means we hand the OS pixels
    # where it expects points, or scale twice - and the window really is wider
    # than designed on scaled screens.
    #
    # SLINT_SCALE_FACTOR is Slint's own override (i-slint-backend-winit,
    # winitwindowadapter.rs: the value replaces winit_window.scale_factor() AND
    # converts the logical sizes in the window attributes), so the app builds its
    # window exactly as it would on a monitor with that scale.
    #
    # HEIGHT IS NOT JUDGED HERE, on purpose. It is computed from the screen
    # height, and the screen stays a 100% one under this override - so the height
    # comes out scaled by sf and off-screen at 200%. The honest height check is a
    # pure-function one and lives in the tests of main.rs.
    Stop-Process -Name 'bemyvpn-gui' -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2
    # The reference is the app's OWN width at 100%, not a number copied from the
    # source: the layout width lives in main.rs (and is deliberately not the same
    # on every OS), and a second copy here would go stale the day it changes.
    # The statement being checked is "the same in points at every scale".
    $ref = 0.0
    $wrong = @()
    $n = 0
    foreach ($sf in 1.0, 1.25, 1.5, 2.0) {
        $n++
        $env:SLINT_SCALE_FACTOR = $sf.ToString($inv)
        $p2 = Start-Bg $gui @() "dpi$n" $false
        $f2 = Wait-Window $p2.Id
        Start-Sleep -Seconds 4
        $c2 = New-Object Win32+RECT
        [void][Win32]::GetClientRect($f2[0], [ref]$c2)
        $osdpi = [Win32]::GetDpiForWindow($f2[0]) / 96.0
        $wpt = $c2.Right / $sf
        Write-Host ("SCALE {0}: client {1}x{2} px = {3} pt wide (OS scale {4})" -f
            (Fmt $sf), $c2.Right, $c2.Bottom, (Fmt $wpt), (Fmt $osdpi))
        if ($ref -eq 0.0) { $ref = $wpt }
        elseif ([Math]::Abs($wpt - $ref) -gt 1.0) {
            $wrong += ("scale {0}: {1} pt instead of {2}" -f (Fmt $sf), (Fmt $wpt), (Fmt $ref))
        }
        Stop-Process -Name 'bemyvpn-gui' -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 2
    }
    $env:SLINT_SCALE_FACTOR = $null
    if ($wrong.Count -gt 0) { throw ("window width changes with the screen scale: " + ($wrong -join '; ')) }
    Write-Host ("OK: {0} pt wide at 100%, 125%, 150% and 200%" -f (Fmt $ref))

    if ($shotError) {
        if ($strict) { throw $shotError }
        Write-Host "shots are not judged on this runner (its desktop is not ours): $($shotError.Exception.Message)"
    }
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
