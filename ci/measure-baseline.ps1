[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string] $ExePath,

    [ValidateRange(5, 100)]
    [int] $ColdStarts = 5,

    [ValidateRange(1, 600)]
    [int] $WarmupSeconds = 60,

    [ValidateRange(1, 3600)]
    [int] $SampleSeconds = 600,

    [ValidateRange(1, 60)]
    [int] $SampleIntervalSeconds = 5
)

$ErrorActionPreference = 'Stop'
$resolvedExe = (Resolve-Path -LiteralPath $ExePath).Path
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())

if (-not ('Claudometer.MeasurementNative' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

namespace Claudometer {
    public static class MeasurementNative {
        [DllImport("user32.dll")]
        public static extern uint GetGuiResources(IntPtr process, uint flags);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern IntPtr CreateEvent(
            IntPtr attributes,
            bool manualReset,
            bool initialState,
            string name);

        [DllImport("kernel32.dll", SetLastError = true)]
        public static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);

        [DllImport("kernel32.dll", SetLastError = true)]
        public static extern bool CloseHandle(IntPtr handle);
    }
}
'@
}

function Start-DemoProcess {
    param(
        [bool] $Hidden,
        [string] $ReadyEventName
    )

    $sandbox = Join-Path $tempRoot ('claudometer-baseline-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $sandbox | Out-Null
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $resolvedExe
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $Hidden
    $start.WindowStyle = if ($Hidden) {
        [Diagnostics.ProcessWindowStyle]::Hidden
    } else {
        [Diagnostics.ProcessWindowStyle]::Normal
    }
    $start.ArgumentList.Add('--demo=both')
    if ($Hidden) {
        $start.ArgumentList.Add('--demo-hidden')
    }
    if ($ReadyEventName) {
        $start.ArgumentList.Add("--demo-ready-event=$ReadyEventName")
    }
    foreach ($name in @('APPDATA', 'LOCALAPPDATA', 'USERPROFILE', 'CLAUDE_CONFIG_DIR', 'CODEX_HOME')) {
        $start.Environment[$name] = $sandbox
    }
    [pscustomobject]@{
        Process = [Diagnostics.Process]::Start($start)
        Sandbox = $sandbox
    }
}

function Stop-DemoProcess {
    param($Run)

    if ($null -ne $Run.Process -and -not $Run.Process.HasExited) {
        $Run.Process.Kill()
        $Run.Process.WaitForExit()
    }
    $resolvedSandbox = [IO.Path]::GetFullPath($Run.Sandbox)
    if (-not $resolvedSandbox.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -or
        -not ([IO.Path]::GetFileName($resolvedSandbox)).StartsWith('claudometer-baseline-')) {
        throw "Refusing to remove unexpected baseline directory '$resolvedSandbox'."
    }
    Remove-Item -LiteralPath $resolvedSandbox -Recurse -Force
}

function Get-Percentile {
    param(
        [double[]] $Values,
        [double] $Percentile
    )
    $sorted = @($Values | Sort-Object)
    $rank = [Math]::Ceiling($Percentile * $sorted.Count) - 1
    $sorted[[Math]::Max(0, $rank)]
}

function Get-ProcessSample {
    param([Diagnostics.Process] $Process)

    $Process.Refresh()
    if ($Process.HasExited) {
        throw "Demo process $($Process.Id) exited during measurement."
    }
    $perf = Get-CimInstance Win32_PerfRawData_PerfProc_Process -Filter "IDProcess=$($Process.Id)"
    if ($null -eq $perf) {
        throw "No performance counters found for demo process $($Process.Id)."
    }
    [pscustomobject]@{
        PrivateWorkingSetBytes = [int64] $perf.WorkingSetPrivate
        GdiHandles = [Claudometer.MeasurementNative]::GetGuiResources($Process.Handle, 0)
        UserHandles = [Claudometer.MeasurementNative]::GetGuiResources($Process.Handle, 1)
        TcpConnections = @(Get-NetTCPConnection -OwningProcess $Process.Id -ErrorAction SilentlyContinue).Count
    }
}

$startupMilliseconds = [System.Collections.Generic.List[double]]::new()
for ($run = 0; $run -lt $ColdStarts; $run++) {
    $readyEventName = 'Local\Claudometer.DemoReady.' + [guid]::NewGuid().ToString('N')
    $readyEvent = [Claudometer.MeasurementNative]::CreateEvent(
        [IntPtr]::Zero,
        $true,
        $false,
        $readyEventName
    )
    if ($readyEvent -eq [IntPtr]::Zero) {
        throw 'Could not create the demo readiness event.'
    }
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $demo = Start-DemoProcess -Hidden $false -ReadyEventName $readyEventName
    try {
        $wait = [Claudometer.MeasurementNative]::WaitForSingleObject($readyEvent, 10000)
        if ($wait -ne 0) {
            throw "Demo tray readiness was not signaled (wait result $wait)."
        }
        $clock.Stop()
        $startupMilliseconds.Add($clock.Elapsed.TotalMilliseconds)
    } finally {
        Stop-DemoProcess $demo
        [Claudometer.MeasurementNative]::CloseHandle($readyEvent) | Out-Null
    }
    Start-Sleep -Milliseconds 250
}

$hidden = Start-DemoProcess -Hidden $true
$visible = Start-DemoProcess -Hidden $false
try {
    Start-Sleep -Seconds $WarmupSeconds
    $hidden.Process.Refresh()
    $visible.Process.Refresh()
    $cpuStartHidden = $hidden.Process.TotalProcessorTime
    $cpuStartVisible = $visible.Process.TotalProcessorTime
    $sampleClock = [Diagnostics.Stopwatch]::StartNew()
    $hiddenSamples = [System.Collections.Generic.List[object]]::new()
    $visibleSamples = [System.Collections.Generic.List[object]]::new()
    do {
        $hiddenSamples.Add((Get-ProcessSample $hidden.Process))
        $visibleSamples.Add((Get-ProcessSample $visible.Process))
        Start-Sleep -Seconds $SampleIntervalSeconds
    } while ($sampleClock.Elapsed.TotalSeconds -lt $SampleSeconds)
    $sampleClock.Stop()
    $hidden.Process.Refresh()
    $visible.Process.Refresh()
    $cpuHidden = ($hidden.Process.TotalProcessorTime - $cpuStartHidden).TotalSeconds
    $cpuVisible = ($visible.Process.TotalProcessorTime - $cpuStartVisible).TotalSeconds
    $logicalProcessors = [Environment]::ProcessorCount

    function Summarize-Process {
        param(
            $Samples,
            [double] $CpuSeconds
        )
        $private = [double[]] @($Samples | ForEach-Object { $_.PrivateWorkingSetBytes })
        $gdi = [double[]] @($Samples | ForEach-Object { $_.GdiHandles })
        $user = [double[]] @($Samples | ForEach-Object { $_.UserHandles })
        [ordered]@{
            sample_count = $Samples.Count
            private_working_set_bytes_median = [int64] (Get-Percentile $private 0.50)
            private_working_set_bytes_p95 = [int64] (Get-Percentile $private 0.95)
            private_working_set_bytes_max = [int64] (($private | Measure-Object -Maximum).Maximum)
            gdi_handles_p95 = [int] (Get-Percentile $gdi 0.95)
            gdi_handles_max = [int] (($gdi | Measure-Object -Maximum).Maximum)
            user_handles_p95 = [int] (Get-Percentile $user 0.95)
            user_handles_max = [int] (($user | Measure-Object -Maximum).Maximum)
            idle_cpu_percent = [Math]::Round(
                100.0 * $CpuSeconds / ($sampleClock.Elapsed.TotalSeconds * $logicalProcessors),
                4
            )
            active_tcp_connections_max = [int] (($Samples.TcpConnections | Measure-Object -Maximum).Maximum)
        }
    }

    $operatingSystem = Get-CimInstance Win32_OperatingSystem
    $computer = Get-CimInstance Win32_ComputerSystem
    $processor = Get-CimInstance Win32_Processor | Select-Object -First 1
    $artifact = Get-Item -LiteralPath $resolvedExe
    $result = [ordered]@{
        measured_at_utc = [DateTime]::UtcNow.ToString('o')
        executable = $artifact.Name
        executable_bytes = $artifact.Length
        reference_machine = [ordered]@{
            os_caption = $operatingSystem.Caption
            os_version = $operatingSystem.Version
            os_build = $operatingSystem.BuildNumber
            os_architecture = $operatingSystem.OSArchitecture
            manufacturer = $computer.Manufacturer
            model = $computer.Model
            logical_processors = $logicalProcessors
            physical_memory_bytes = [int64] $computer.TotalPhysicalMemory
            processor = $processor.Name
        }
        method = [ordered]@{
            mode = '--demo=both (isolated empty profile; no refresh workers)'
            cold_start_runs = $ColdStarts
            readiness = 'nonce-bound kernel event signaled immediately after Shell_NotifyIcon(NIM_ADD) returns'
            warmup_seconds = $WarmupSeconds
            sample_seconds = $SampleSeconds
            sample_interval_seconds = $SampleIntervalSeconds
            percentile = 'nearest-rank'
        }
        startup_ms = [ordered]@{
            samples = $startupMilliseconds
            median = [Math]::Round((Get-Percentile $startupMilliseconds.ToArray() 0.50), 3)
            p95 = [Math]::Round((Get-Percentile $startupMilliseconds.ToArray() 0.95), 3)
        }
        hidden = Summarize-Process $hiddenSamples $cpuHidden
        visible_two_provider = Summarize-Process $visibleSamples $cpuVisible
    }
    $result | ConvertTo-Json -Depth 8
} finally {
    Stop-DemoProcess $hidden
    Stop-DemoProcess $visible
}
