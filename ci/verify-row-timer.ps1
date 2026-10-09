[CmdletBinding()]
param([Parameter(Mandatory)][string] $ExePath,
      [string] $OutputPath = 'target/row-01/timer')

$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Path $OutputPath -Force | Out-Null
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class RowTimer {
    public delegate bool EnumProc(IntPtr hwnd, IntPtr lparam);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback, IntPtr lparam);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder name, int length);
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    public static IntPtr Main(uint pid) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((hwnd, lparam) => {
            uint owner; GetWindowThreadProcessId(hwnd, out owner);
            var name = new StringBuilder(128); GetClassName(hwnd, name, name.Capacity);
            if (owner == pid && name.ToString() == "Claudometer.Main") { found = hwnd; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
'@
$start = [Diagnostics.ProcessStartInfo]::new((Resolve-Path -LiteralPath $ExePath).Path)
$start.UseShellExecute = $false
$start.ArgumentList.Add('--demo=both')
$app = [Diagnostics.Process]::Start($start)
try {
    winapp ui wait-for Usage0 -a $app.Id -t 5000 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'No demo rows' }
    winapp ui wait-for FooterRefresh -a $app.Id --value 'Next update in 5m' -t 5000 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Initial freshness deadline missing' }
    $window = @(winapp ui list-windows -a $app.Id --json | ConvertFrom-Json)[0]
    $main = [RowTimer]::Main($app.Id)
    if ($main -eq [IntPtr]::Zero) { throw 'No demo main window' }
    $scale = [RowTimer]::GetDpiForWindow([IntPtr]$window.hwnd) / 96.0
    $click = ([int](100 * $scale) -shl 16) -bor [int](250 * $scale)
    [RowTimer]::SendMessage([IntPtr]$window.hwnd, 0x202, [IntPtr]::Zero, [IntPtr]$click) | Out-Null
    winapp ui screenshot -a $app.Id --capture-screen -o (Join-Path $OutputPath 'before.png') | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Initial capture failed' }
    # Runtime observation, not a unit test: permit two real 30-second ticks.
    Start-Sleep -Seconds 33
    Start-Sleep -Seconds 33
    winapp ui wait-for FooterRefresh -a $app.Id --value 'Next update in 4m' -t 5000 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Minute-granular footer did not advance' }
    winapp ui wait-for Usage4 -a $app.Id --value 'Updated 1m ago' -t 5000 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Accessible observation age did not advance' }
    winapp ui screenshot -a $app.Id --capture-screen -o (Join-Path $OutputPath 'after.png') | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Tick capture failed' }
    # Timer ownership stays on the application's thread. Drive hide/reopen
    # through the tray path; the source check pins timer removal below.
    [RowTimer]::SendMessage($main, 0x8001, [IntPtr]::Zero, [IntPtr]0x400) | Out-Null
    [RowTimer]::SendMessage($main, 0x8001, [IntPtr]::Zero, [IntPtr]0x400) | Out-Null
    if (-not [RowTimer]::IsWindowVisible([IntPtr]$window.hwnd)) { throw 'Flyout did not reopen' }
    [RowTimer]::SendMessage($main, 0x8001, [IntPtr]::Zero, [IntPtr]0x400) | Out-Null
    if ([RowTimer]::IsWindowVisible([IntPtr]$window.hwnd)) { throw 'Flyout did not hide' }
    $source = Get-Content -LiteralPath (Join-Path $PSScriptRoot '../src/main.rs') -Raw
    if ([regex]::Matches($source, 'TIMER_TICK,\s*30_000,').Count -ne 2) { throw 'Unexpected visible tick interval' }
    if ($source -notmatch '(?s)unsafe fn hide_flyout\(\).*?KillTimer\([^;]*TIMER_TICK\)') { throw 'Hide path does not remove the tick timer' }
    if ($source -notmatch '(?s)unsafe fn render_flyout_current\(\).*?if !IsWindowVisible\(fh\).as_bool\(\) \{\s*return;') { throw 'Hidden repaint guard is missing' }
    [pscustomobject]@{ visible = 'countdown captures, next update 5m to 4m, Updated 1m ago'; hidden = 'tray hide/reopen and source guard'; interval_ms = 30000; observed_seconds = 66 } |
        ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputPath 'results.json')
    Write-Output 'Row timer passed: tray hide/reopen, 30-second interval and hidden guards. Review before/after captures for countdown progression.'
} finally {
    if (-not $app.HasExited) { $app.Kill(); $app.WaitForExit() }
}
