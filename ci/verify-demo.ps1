[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string] $ExePath
)

$ErrorActionPreference = 'Stop'
$resolvedExe = (Resolve-Path -LiteralPath $ExePath).Path
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$sandbox = Join-Path $tempRoot ('claudometer-demo-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $sandbox | Out-Null

function Get-OwnedRegistryState {
    $runValue = Get-ItemPropertyValue `
        -LiteralPath 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' `
        -Name 'Claudometer' `
        -ErrorAction SilentlyContinue
    $aumid = Get-ItemProperty `
        -LiteralPath 'HKCU:\Software\Classes\AppUserModelId\Claudometer' `
        -ErrorAction SilentlyContinue
    [pscustomobject]@{
        Run = $runValue
        AumidDisplayName = $aumid.DisplayName
        AumidIconUri = $aumid.IconUri
    } | ConvertTo-Json -Compress
}

$registryBefore = Get-OwnedRegistryState
$powerBefore = (& powercfg.exe /getactivescheme 2>&1 | Out-String)
$lidPolicyBefore = (& powercfg.exe /query SCHEME_CURRENT `
    4f971e89-eebd-4455-a8de-9e59040e7347 `
    5ca83367-6e45-459f-a27b-476b1d01c936 2>&1 | Out-String)
$process = $null
try {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $resolvedExe
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
    $start.ArgumentList.Add('--demo=both')
    $start.ArgumentList.Add('--demo-hidden')
    foreach ($name in @('APPDATA', 'LOCALAPPDATA', 'USERPROFILE', 'CLAUDE_CONFIG_DIR', 'CODEX_HOME')) {
        $start.Environment[$name] = $sandbox
    }

    $process = [Diagnostics.Process]::Start($start)
    Start-Sleep -Milliseconds 1500
    if ($process.HasExited) {
        throw "Demo process exited unexpectedly with code $($process.ExitCode)."
    }

    $children = Get-CimInstance Win32_Process -Filter "ParentProcessId = $($process.Id)"
    if ($children) {
        throw 'Demo mode launched a child process.'
    }
    $connections = Get-NetTCPConnection -OwningProcess $process.Id -ErrorAction SilentlyContinue
    if ($connections) {
        throw 'Demo mode opened a TCP connection.'
    }
    if (Get-ChildItem -LiteralPath $sandbox -Force) {
        throw 'Demo mode wrote to its isolated profile.'
    }
    if ((Get-OwnedRegistryState) -cne $registryBefore) {
        throw 'Demo mode changed Claudometer-owned registry state.'
    }
    $powerAfter = (& powercfg.exe /getactivescheme 2>&1 | Out-String)
    if ($powerAfter -cne $powerBefore) {
        throw 'Demo mode changed the active power scheme.'
    }
    $lidPolicyAfter = (& powercfg.exe /query SCHEME_CURRENT `
        4f971e89-eebd-4455-a8de-9e59040e7347 `
        5ca83367-6e45-459f-a27b-476b1d01c936 2>&1 | Out-String)
    if ($lidPolicyAfter -cne $lidPolicyBefore) {
        throw 'Demo mode changed AC/DC lid policy.'
    }

    Write-Output 'Demo safety verification passed: no files, registry changes, child processes, TCP connections, active-scheme change, or AC/DC lid-policy change.'
} finally {
    if ($null -ne $process -and -not $process.HasExited) {
        $process.Kill()
        $process.WaitForExit()
    }
    $resolvedSandbox = [IO.Path]::GetFullPath($sandbox)
    if (-not $resolvedSandbox.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -or
        -not ([IO.Path]::GetFileName($resolvedSandbox)).StartsWith('claudometer-demo-')) {
        throw "Refusing to remove unexpected demo directory '$resolvedSandbox'."
    }
    Remove-Item -LiteralPath $resolvedSandbox -Recurse -Force
}
