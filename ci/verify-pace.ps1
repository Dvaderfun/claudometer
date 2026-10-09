[CmdletBinding()]
param([Parameter(Mandatory)][string] $ExePath,
      [string] $OutputPath = 'target/pace-01/ui')

$ErrorActionPreference = 'Stop'
$resolvedExe = (Resolve-Path -LiteralPath $ExePath).Path
New-Item -ItemType Directory -Path $OutputPath -Force | Out-Null
$results = [Collections.Generic.List[object]]::new()
function Invoke-Check([string] $Name, [scriptblock] $Action) {
    & $Action | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Failed: $Name" }
    $results.Add([pscustomobject]@{ check = $Name; result = 'pass' })
}

foreach ($mode in @('dark', 'light', 'contrast', 'settings')) {
    $start = [Diagnostics.ProcessStartInfo]::new($resolvedExe)
    $start.UseShellExecute = $false
    $start.ArgumentList.Add('--demo')
    $start.ArgumentList.Add($(if ($mode -eq 'settings') { 'settings' } else { 'both' }))
    if ($mode -eq 'light') { $start.ArgumentList.Add('--demo-light') }
    if ($mode -eq 'contrast') { $start.ArgumentList.Add('--demo-contrast') }
    $app = [Diagnostics.Process]::Start($start)
    try {
        if ($mode -eq 'settings') {
            Invoke-Check 'Settings ready' { winapp ui wait-for ClaudeAccount -a $app.Id -t 5000 }
            Invoke-Check 'Pace toggle focus' { winapp ui focus PaceColors -a $app.Id }
            Invoke-Check 'Pace toggle discoverable and on' {
                winapp ui wait-for PaceColors -a $app.Id --value on -t 5000
            }
            Invoke-Check 'Pace Toggle pattern' { winapp ui invoke PaceColors -a $app.Id }
            Invoke-Check 'Pace keyboard path' { winapp ui send-keys 'space enter' -a $app.Id }
            Invoke-Check 'Pace visible focus' { winapp ui focus PaceColors -a $app.Id }
            # Demo dispatch is guarded; these calls prove availability, not mutation.
            winapp ui get-property PaceColors -a $app.Id --json |
                Set-Content -LiteralPath (Join-Path $OutputPath 'pace-toggle.json')
        } else {
            Invoke-Check "$mode OnTrack name" {
                winapp ui wait-for Usage0 -a $app.Id --value 'On track at current pace' --contains -t 5000
            }
            Invoke-Check "$mode Tight name" {
                winapp ui wait-for Usage1 -a $app.Id --value 'Tight at current pace' --contains -t 5000
            }
            Invoke-Check "$mode Over name" {
                winapp ui wait-for Usage2 -a $app.Id --value 'Over at current pace' --contains -t 5000
            }
        }
        winapp ui inspect -a $app.Id --json -d 8 |
            Set-Content -LiteralPath (Join-Path $OutputPath "$mode-tree.json")
        Invoke-Check "$mode capture" {
            winapp ui screenshot -a $app.Id --capture-screen -o (Join-Path $OutputPath "$mode.png")
        }
    } finally {
        if (-not $app.HasExited) { $app.Kill(); $app.WaitForExit() }
    }
}
$results | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputPath 'results.json')
$results | Format-Table -AutoSize
