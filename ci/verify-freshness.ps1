[CmdletBinding()]
param([Parameter(Mandatory)][string] $ExePath,
      [string] $OutputPath = 'target/fresh-01/ui')

$ErrorActionPreference = 'Stop'
$resolvedExe = (Resolve-Path -LiteralPath $ExePath).Path
New-Item -ItemType Directory -Path $OutputPath -Force | Out-Null
$results = [Collections.Generic.List[object]]::new()
function Invoke-Check([string] $Name, [scriptblock] $Action) {
    & $Action | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Failed: $Name" }
    $results.Add([pscustomobject]@{ check = $Name; result = 'pass' })
}
foreach ($mode in @('loading', 'stale', 'cooldown', 'error', 'both', 'light', 'contrast', 'many')) {
    $sandbox = Join-Path ([IO.Path]::GetTempPath()) ('claudometer-fresh-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $sandbox | Out-Null
    $start = [Diagnostics.ProcessStartInfo]::new($resolvedExe)
    $start.UseShellExecute = $false
    $start.ArgumentList.Add('--demo')
    $start.ArgumentList.Add($(if ($mode -in @('light', 'contrast')) { 'stale' } else { $mode }))
    if ($mode -eq 'light') { $start.ArgumentList.Add('--demo-light') }
    if ($mode -eq 'contrast') { $start.ArgumentList.Add('--demo-contrast') }
    foreach ($name in @('APPDATA', 'LOCALAPPDATA', 'USERPROFILE', 'CLAUDE_CONFIG_DIR', 'CODEX_HOME')) { $start.Environment[$name] = $sandbox }
    $app = [Diagnostics.Process]::Start($start)
    try {
        $token = switch ($mode) {
            loading { 'Updating…' }
            cooldown { 'Paused by provider' }
            error { 'Offline' }
            both { 'Session (5h)' }
            many { 'Session (5h)' }
            default { 'Outdated' }
        }
        Invoke-Check "$mode status exposed" { winapp ui wait-for Usage0 -a $app.Id --value $token --contains -t 5000 }
        if ($mode -in @('stale', 'light', 'contrast')) {
            $help = (winapp ui get-property Usage0 -a $app.Id --json | ConvertFrom-Json).properties.HelpText
            if ($help -notmatch 'Last updated 3h ago' -or $help -notmatch 'Compatibility') { throw 'Outdated age/source missing from UIA' }
            $results.Add([pscustomobject]@{ check = "$mode UIA age and source"; result = 'pass' })
            Invoke-Check "$mode last values preserved" { winapp ui wait-for Usage1 -a $app.Id --value '36% used' --contains -t 5000 }
            Invoke-Check "$mode next update" { winapp ui wait-for FooterRefresh -a $app.Id --value 'Next update in 4m' -t 5000 }
        }
        if ($mode -eq 'cooldown') {
            Invoke-Check 'Cooldown deadline' { winapp ui wait-for FooterRefresh -a $app.Id --value 'Retry at ' --contains -t 5000 }
            $enabled = (winapp ui get-property FooterRefresh -a $app.Id --json | ConvertFrom-Json).properties.IsEnabled
            if ($enabled -ne $false -and $enabled -ne 'False') { throw 'Cooldown action is enabled' }
            $results.Add([pscustomobject]@{ check = 'Cooldown action disabled'; result = 'pass' })
        } else {
            Invoke-Check "$mode footer focus" { winapp ui focus FooterRefresh -a $app.Id }
            Invoke-Check "$mode footer Space/Enter" { winapp ui send-keys 'space enter' -a $app.Id }
            Invoke-Check "$mode footer Invoke" { winapp ui invoke FooterRefresh -a $app.Id }
            Invoke-Check "$mode focus before click" { winapp ui focus FooterRefresh -a $app.Id }
            Invoke-Check "$mode footer click" { winapp ui click FooterRefresh -a $app.Id }
        }
        if ($mode -eq 'many') {
            Invoke-Check 'Scrolled footer focus' { winapp ui focus FooterRefresh -a $app.Id }
            $offscreen = (winapp ui get-property FooterRefresh -a $app.Id --json | ConvertFrom-Json).properties.IsOffscreen
            if ($offscreen -ne $false -and $offscreen -ne 'False') { throw 'Scrolled footer is offscreen' }
            $results.Add([pscustomobject]@{ check = 'Scrolled footer remains visible'; result = 'pass' })
        }
        winapp ui inspect -a $app.Id --json -d 8 | Set-Content -LiteralPath (Join-Path $OutputPath "$mode-tree.json")
        Invoke-Check "$mode capture" { winapp ui screenshot -a $app.Id --capture-screen -o (Join-Path $OutputPath "$mode.png") }
        if (Get-ChildItem -LiteralPath $sandbox -Force) { throw 'Freshness demo wrote its profile' }
        if (Get-NetTCPConnection -OwningProcess $app.Id -ErrorAction SilentlyContinue) { throw 'Freshness demo opened TCP' }
        if (Get-CimInstance Win32_Process -Filter "ParentProcessId = $($app.Id)") { throw 'Freshness demo launched a child' }
        $results.Add([pscustomobject]@{ check = "$mode no profile writes/TCP/child"; result = 'pass' })
    } finally {
        if (-not $app.HasExited) { $app.Kill(); $app.WaitForExit() }
        $resolvedSandbox = [IO.Path]::GetFullPath($sandbox)
        if (-not $resolvedSandbox.StartsWith([IO.Path]::GetFullPath([IO.Path]::GetTempPath()), [StringComparison]::OrdinalIgnoreCase) -or
            -not ([IO.Path]::GetFileName($resolvedSandbox)).StartsWith('claudometer-fresh-')) { throw "Unexpected sandbox '$resolvedSandbox'" }
        Remove-Item -LiteralPath $resolvedSandbox -Recurse -Force
    }
}
$results | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputPath 'results.json')
$results | Format-Table -AutoSize
