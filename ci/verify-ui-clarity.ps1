[CmdletBinding()]
param([Parameter(Mandatory)][string] $ExePath,
      [string] $OutputPath = 'target/ui-clarity/ui')

$ErrorActionPreference = 'Stop'
$resolvedExe = (Resolve-Path -LiteralPath $ExePath).Path
New-Item -ItemType Directory -Path $OutputPath -Force | Out-Null
$results = [Collections.Generic.List[object]]::new()
function Check([string] $Name, [scriptblock] $Action) {
    & $Action | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Failed: $Name" }
    $results.Add([pscustomobject]@{ check = $Name; result = 'pass' })
}
foreach ($mode in @('both', 'light', 'contrast', 'settings', 'cooldown')) {
    $sandbox = Join-Path ([IO.Path]::GetTempPath()) ('claudometer-clarity-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $sandbox | Out-Null
    $start = [Diagnostics.ProcessStartInfo]::new($resolvedExe)
    $start.UseShellExecute = $false
    $start.ArgumentList.Add('--demo')
    $start.ArgumentList.Add($(if ($mode -in @('light', 'contrast')) { 'both' } else { $mode }))
    if ($mode -eq 'light') { $start.ArgumentList.Add('--demo-light') }
    if ($mode -eq 'contrast') { $start.ArgumentList.Add('--demo-contrast') }
    foreach ($name in @('APPDATA', 'LOCALAPPDATA', 'USERPROFILE', 'CLAUDE_CONFIG_DIR', 'CODEX_HOME')) { $start.Environment[$name] = $sandbox }
    $app = [Diagnostics.Process]::Start($start)
    try {
        if ($mode -eq 'settings') {
            Check 'Settings mode ready' { winapp ui wait-for LidOverride -a $app.Id -p Name --value 'Vibecode mode' -t 5000 }
            Check 'Settings mode Invoke on' { winapp ui invoke LidOverride -a $app.Id }
            Check 'Settings mode enabled' { winapp ui wait-for LidOverride -a $app.Id --value on -t 5000 }
            Check 'Settings mode Invoke off' { winapp ui invoke LidOverride -a $app.Id }
            Check 'Settings mode restored' { winapp ui wait-for LidOverride -a $app.Id --value off -t 5000 }
            Check 'Compact diagnostics ready' { winapp ui wait-for CopyDiagnostics -a $app.Id -t 5000 }
            $diag = (winapp ui get-property CopyDiagnostics -a $app.Id --json | ConvertFrom-Json).properties
            if ($diag.HelpText -notmatch 'Attempt:|Windows build:') { throw 'Full diagnostics missing from UIA' }
            $results.Add([pscustomobject]@{ check = 'Full sanitized diagnostics retained'; result = 'pass' })
            Check 'Compact diagnostics focus' { winapp ui focus CopyDiagnostics -a $app.Id }
            Check 'Copy full diagnostics Invoke' { winapp ui invoke CopyDiagnostics -a $app.Id }
            Check 'Compact diagnostics screenshot' { winapp ui screenshot -a $app.Id --capture-screen -o (Join-Path $OutputPath 'diagnostics.png') }
            Check 'Codex source explanation ready' { winapp ui wait-for CodexAppServer -a $app.Id -p Name --value 'Use Codex CLI for usage' -t 5000 }
            $help = (winapp ui get-property CodexAppServer -a $app.Id --json | ConvertFrom-Json).properties.HelpText
            if ($help -notmatch 'two seconds' -or $help -notmatch 'additional quotas') { throw 'Codex explanation missing' }
            $results.Add([pscustomobject]@{ check = 'Codex effect and speed explained'; result = 'pass' })
            Check 'Codex source focus' { winapp ui focus CodexAppServer -a $app.Id }
        } else {
            Check "$mode Vibecode ready" { winapp ui wait-for KeepAwake -a $app.Id -p Name --value 'Vibecode mode' -t 5000 }
            Check "$mode Vibecode off" { winapp ui wait-for KeepAwake -a $app.Id --value off -t 5000 }
            Check "$mode Vibecode focus" { winapp ui focus KeepAwake -a $app.Id }
            Check "$mode Vibecode Space" { winapp ui send-keys space -a $app.Id }
            Check "$mode Vibecode on" { winapp ui wait-for KeepAwake -a $app.Id --value on -t 5000 }
            Check "$mode active capture" { winapp ui screenshot -a $app.Id --capture-screen -o (Join-Path $OutputPath "$mode-on.png") }
            Check "$mode Vibecode Invoke off" { winapp ui invoke KeepAwake -a $app.Id }
            Check "$mode Vibecode restored off" { winapp ui wait-for KeepAwake -a $app.Id --value off -t 5000 }
            if ($mode -eq 'cooldown') {
                Check 'Compact footer preserves cooldown' { winapp ui wait-for FooterRefresh -a $app.Id --value 'Retry at ' --contains -t 5000 }
            } else {
                Check "$mode footer action preserved" { winapp ui wait-for FooterRefresh -a $app.Id --value 'Next in 5m' -t 5000 }
            }
        }
        Check "$mode capture" { winapp ui screenshot -a $app.Id --capture-screen -o (Join-Path $OutputPath "$mode.png") }
        if (Get-ChildItem -LiteralPath $sandbox -Force) { throw 'UI demo wrote its profile' }
        if (Get-NetTCPConnection -OwningProcess $app.Id -ErrorAction SilentlyContinue) { throw 'UI demo opened TCP' }
        if (Get-CimInstance Win32_Process -Filter "ParentProcessId = $($app.Id)") { throw 'UI demo started a child' }
        $results.Add([pscustomobject]@{ check = "$mode no files/TCP/child"; result = 'pass' })
    } finally {
        if (-not $app.HasExited) { $app.Kill(); $app.WaitForExit() }
        $resolvedSandbox = [IO.Path]::GetFullPath($sandbox)
        if (-not $resolvedSandbox.StartsWith([IO.Path]::GetFullPath([IO.Path]::GetTempPath()), [StringComparison]::OrdinalIgnoreCase) -or
            -not ([IO.Path]::GetFileName($resolvedSandbox)).StartsWith('claudometer-clarity-')) { throw "Unexpected sandbox '$resolvedSandbox'" }
        Remove-Item -LiteralPath $resolvedSandbox -Recurse -Force
    }
}
$results | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputPath 'results.json')
$results | Format-Table -AutoSize
