[CmdletBinding()]
param([Parameter(Mandatory)][string] $ExePath,
      [string] $OutputPath = 'target/row-01/ui')

$ErrorActionPreference = 'Stop'
$resolvedExe = (Resolve-Path -LiteralPath $ExePath).Path
New-Item -ItemType Directory -Path $OutputPath -Force | Out-Null
$results = [Collections.Generic.List[object]]::new()
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class RowMessages {
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X; public int Y; }
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr hwnd, ref Point point);
}
'@
function Invoke-Check([string] $Name, [scriptblock] $Action) {
    & $Action | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Failed: $Name" }
    $results.Add([pscustomobject]@{ check = $Name; result = 'pass' })
}
function Get-RowName($App, [string] $Id = 'Usage0') {
    (winapp ui get-property $Id -a $App.Id --json | ConvertFrom-Json).properties.Name
}
function Click-Row($App, [bool] $Reset, [int] $Index = 0) {
    $window = @(winapp ui list-windows -a $App.Id --json | ConvertFrom-Json)[0]
    $scale = [RowMessages]::GetDpiForWindow([IntPtr]$window.hwnd) / 96.0
    $row = (winapp ui get-property "Usage$Index" -a $App.Id --json | ConvertFrom-Json).properties.BoundingRectangle.Split(',')
    $origin = [RowMessages+Point]::new()
    [RowMessages]::ClientToScreen([IntPtr]$window.hwnd, [ref]$origin) | Out-Null
    $x = if ($Reset) { 250 } else { 40 }
    $y = [double]$row[1] + [double]$row[3] - 8 * $scale - $origin.Y
    $coords = ([int]$y -shl 16) -bor [int]($x * $scale)
    [RowMessages]::SendMessage([IntPtr]$window.hwnd, 0x202, [IntPtr]::Zero, [IntPtr]$coords) | Out-Null
}

foreach ($mode in @('dark', 'light', 'contrast', 'settings', 'claude-only', 'codex-only', 'many')) {
    $sandbox = Join-Path ([IO.Path]::GetTempPath()) ('claudometer-row-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $sandbox | Out-Null
    $start = [Diagnostics.ProcessStartInfo]::new($resolvedExe)
    $start.UseShellExecute = $false
    $start.ArgumentList.Add('--demo')
    $start.ArgumentList.Add($(if ($mode -in @('settings', 'claude-only', 'codex-only', 'many')) { $mode } else { 'both' }))
    if ($mode -eq 'light') { $start.ArgumentList.Add('--demo-light') }
    if ($mode -eq 'contrast') { $start.ArgumentList.Add('--demo-contrast') }
    foreach ($name in @('APPDATA', 'LOCALAPPDATA', 'USERPROFILE', 'CLAUDE_CONFIG_DIR', 'CODEX_HOME')) { $start.Environment[$name] = $sandbox }
    $app = [Diagnostics.Process]::Start($start)
    try {
        if ($mode -eq 'settings') {
            Invoke-Check 'Settings choices ready' { winapp ui wait-for QuotaDisplay -a $app.Id --value 'Quota display, Used' -t 5000 }
            Invoke-Check 'Quota keyboard focus' { winapp ui focus QuotaDisplay -a $app.Id }
            Invoke-Check 'Quota Space action' { winapp ui send-keys 'space' -a $app.Id }
            Invoke-Check 'Quota keyboard changed to Left' { winapp ui wait-for QuotaDisplay -a $app.Id --value 'Quota display, Left' -t 5000 }
            Invoke-Check 'Quota Invoke action' { winapp ui invoke QuotaDisplay -a $app.Id }
            Invoke-Check 'Quota restored to Used' { winapp ui wait-for QuotaDisplay -a $app.Id --value 'Quota display, Used' -t 5000 }
            Invoke-Check 'Reset keyboard focus' { winapp ui focus ResetFormat -a $app.Id }
            Invoke-Check 'Reset Enter action' { winapp ui send-keys 'enter' -a $app.Id }
            Invoke-Check 'Reset changed to Countdown' { winapp ui wait-for ResetFormat -a $app.Id --value 'Reset format, Countdown' -t 5000 }
            Invoke-Check 'Reset Invoke action' { winapp ui invoke ResetFormat -a $app.Id }
            Invoke-Check 'Reset restored to Clock' { winapp ui wait-for ResetFormat -a $app.Id --value 'Reset format, Clock' -t 5000 }
            winapp ui focus ResetFormat -a $app.Id | Out-Null
        } elseif ($mode -eq 'claude-only') {
            Invoke-Check 'Claude Not started explanation' { winapp ui wait-for Usage0 -a $app.Id --value 'Not started. The session starts with your first message.' --contains -t 5000 }
        } elseif ($mode -eq 'codex-only') {
            Invoke-Check 'Codex rows ready' { winapp ui wait-for Usage0 -a $app.Id -t 5000 }
            if ((Get-RowName $app) -match 'Not started|first message') { throw 'Codex inferred Not started' }
            $results.Add([pscustomobject]@{ check = 'Codex does not infer Not started'; result = 'pass' })
        } elseif ($mode -eq 'many') {
            Invoke-Check 'Many rows ready' { winapp ui wait-for Usage17 -a $app.Id -t 5000 }
            Invoke-Check 'Many rows scroll' { winapp ui send-keys 'end' -a $app.Id }
            Click-Row $app $false 17
            Invoke-Check 'Scrolled row value click applies everywhere' { winapp ui wait-for Usage17 -a $app.Id --value '20% left' --contains -t 5000 }
            Click-Row $app $true 17
            Invoke-Check 'Scrolled row reset click applies everywhere' { winapp ui wait-for Usage17 -a $app.Id --value 'resets in ' --contains -t 5000 }
        } else {
            Invoke-Check "$mode used default" { winapp ui wait-for Usage0 -a $app.Id --value '36% used, resets ' --contains -t 5000 }
            Click-Row $app $false
            Invoke-Check "$mode value click changes Claude" { winapp ui wait-for Usage0 -a $app.Id --value '64% left' --contains -t 5000 }
            Invoke-Check "$mode value applies to Codex" { winapp ui wait-for Usage2 -a $app.Id --value '36% left' --contains -t 5000 }
            Click-Row $app $true
            Invoke-Check "$mode countdown session" { winapp ui wait-for Usage0 -a $app.Id --value 'resets in 2h' --contains -t 5000 }
            Invoke-Check "$mode countdown weekly days" { winapp ui wait-for Usage1 -a $app.Id --value 'resets in 2d' --contains -t 5000 }
            Invoke-Check "$mode countdown pace note" { winapp ui wait-for Usage2 -a $app.Id --value 'Limit in ' --contains -t 5000 }
            winapp ui screenshot -a $app.Id --capture-screen -o (Join-Path $OutputPath "$mode-countdown.png") | Out-Null
            Click-Row $app $false
            Click-Row $app $true
            Invoke-Check "$mode clicks restore defaults" { winapp ui wait-for Usage0 -a $app.Id --value '36% used, resets ' --contains -t 5000 }
            if ((Get-RowName $app) -match 'resets in ') { throw 'Clock not restored' }
        }
        winapp ui inspect -a $app.Id --json -d 8 | Set-Content -LiteralPath (Join-Path $OutputPath "$mode-tree.json")
        Invoke-Check "$mode capture" { winapp ui screenshot -a $app.Id --capture-screen -o (Join-Path $OutputPath "$mode.png") }
        if (Get-ChildItem -LiteralPath $sandbox -Force) { throw 'Row demo wrote its profile' }
        if (Get-NetTCPConnection -OwningProcess $app.Id -ErrorAction SilentlyContinue) { throw 'Row demo opened TCP' }
        if (Get-CimInstance Win32_Process -Filter "ParentProcessId = $($app.Id)") { throw 'Row demo launched a child' }
        $results.Add([pscustomobject]@{ check = "$mode no profile writes/TCP/child"; result = 'pass' })
    } finally {
        if (-not $app.HasExited) { $app.Kill(); $app.WaitForExit() }
        $resolvedSandbox = [IO.Path]::GetFullPath($sandbox)
        if (-not $resolvedSandbox.StartsWith([IO.Path]::GetFullPath([IO.Path]::GetTempPath()), [StringComparison]::OrdinalIgnoreCase) -or
            -not ([IO.Path]::GetFileName($resolvedSandbox)).StartsWith('claudometer-row-')) { throw "Unexpected sandbox '$resolvedSandbox'" }
        Remove-Item -LiteralPath $resolvedSandbox -Recurse -Force
    }
}
$results | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputPath 'results.json')
$results | Format-Table -AutoSize
