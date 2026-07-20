# caps-led.ps1 — Caps Lock LED as Claude Code status indicator.
# Controls the LED via IOCTL_KEYBOARD_SET_INDICATORS on \Device\KeyboardClassN.
# Does NOT change the real Caps Lock toggle state — typing is unaffected.
#
# Modes:
#   blink : test pattern — 4 slow blinks (manual verification)
#   on    : LED solid on
#   off   : LED off
#   work  : ensure background flasher is running (UserPromptSubmit hook)
#   flash : internal — flash loop while flag file exists
#   done  : stop flasher, LED solid on (Stop hook)
#   end   : stop flasher, LED off (SessionEnd hook)
#   attention : rapid burst then resume flashing (Notification hook — Claude waits for you)

param(
    [Parameter(Position = 0)]
    [ValidateSet('on', 'off', 'blink', 'work', 'flash', 'done', 'end', 'attention')]
    [string]$Mode = 'blink'
)

$ErrorActionPreference = 'SilentlyContinue'
$FlagFile = Join-Path $env:TEMP 'claude-caps-working.flag'

# Kill switch (managed by Claudometer settings): hook modes become no-ops.
# 'end' stays allowed so disabling can clean up; manual on/off/blink stay allowed.
$DisabledMarker = Join-Path $PSScriptRoot 'caps-led.disabled'
if ((Test-Path $DisabledMarker) -and ($Mode -in @('work', 'flash', 'done', 'attention'))) { exit }

$src = @'
using System;
using System.Runtime.InteropServices;

public static class KbdLed {
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool DefineDosDevice(uint dwFlags, string lpDeviceName, string lpTargetPath);
    [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)]
    static extern IntPtr CreateFileW(string lpFileName, uint dwDesiredAccess, uint dwShareMode, IntPtr sa, uint dwCreationDisposition, uint dwFlags, IntPtr hTemplate);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool DeviceIoControl(IntPtr hDevice, uint code, ref KIP inBuf, int inSize, IntPtr outBuf, int outSize, out uint ret, IntPtr ovl);
    [DllImport("kernel32.dll")]
    static extern bool CloseHandle(IntPtr h);

    [StructLayout(LayoutKind.Sequential)]
    struct KIP { public ushort UnitId; public ushort LedFlags; }

    const uint IOCTL_KEYBOARD_SET_INDICATORS = 0x000B0008;
    const ushort KEYBOARD_CAPS_LOCK_ON = 4;

    public static int Set(bool capsOn) {
        int ok = 0;
        for (int i = 0; i < 4; i++) {
            string dos = "ClaudeCapsLed" + i;
            if (!DefineDosDevice(1, dos, "\\Device\\KeyboardClass" + i)) continue;
            IntPtr h = CreateFileW("\\\\.\\" + dos, 0x40000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
            if (h != new IntPtr(-1)) {
                KIP k; k.UnitId = 0; k.LedFlags = (ushort)(capsOn ? KEYBOARD_CAPS_LOCK_ON : 0);
                uint r;
                if (DeviceIoControl(h, IOCTL_KEYBOARD_SET_INDICATORS, ref k, Marshal.SizeOf(typeof(KIP)), IntPtr.Zero, 0, out r, IntPtr.Zero)) ok++;
                CloseHandle(h);
            }
            DefineDosDevice(7, dos, "\\Device\\KeyboardClass" + i);
        }
        return ok;
    }
}
'@

# 'work' only touches the flag file and spawns the flasher — no LED write, skip compile
if ($Mode -ne 'work' -and -not ('KbdLed' -as [type])) { Add-Type -TypeDefinition $src }

function Set-CapsLed([bool]$On) { [void][KbdLed]::Set($On) }

switch ($Mode) {
    'on'  { Set-CapsLed $true }
    'off' { Set-CapsLed $false }

    'blink' {
        # Manual test: 4 slow blinks, 1s on / 1s off
        1..4 | ForEach-Object {
            Set-CapsLed $true;  Start-Sleep -Milliseconds 1000
            Set-CapsLed $false; Start-Sleep -Milliseconds 1000
        }
    }

    'work' {
        # Flasher alive = flag exists and touched within last 5s (flash loop touches it each cycle)
        $alive = (Test-Path $FlagFile) -and
                 (((Get-Date) - (Get-Item $FlagFile).LastWriteTime).TotalSeconds -lt 5)
        if (-not $alive) {
            New-Item -ItemType File -Path $FlagFile -Force | Out-Null
            Start-Process -WindowStyle Hidden powershell.exe -ArgumentList @(
                '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath, 'flash')
        }
    }

    'flash' {
        while (Test-Path $FlagFile) {
            (Get-Item $FlagFile).LastWriteTime = Get-Date   # heartbeat for 'work'
            Set-CapsLed $true
            Start-Sleep -Milliseconds 350
            if (-not (Test-Path $FlagFile)) { break }
            Set-CapsLed $false
            Start-Sleep -Milliseconds 350
        }
    }

    'done' {
        Remove-Item $FlagFile -Force
        Start-Sleep -Milliseconds 800   # let flasher loop exit before final write
        Set-CapsLed $true               # solid = done
    }

    'end' {
        Remove-Item $FlagFile -Force
        Start-Sleep -Milliseconds 800
        Set-CapsLed $false
    }

    'attention' {
        # Claude waits for permission/input: stop flasher, rapid burst, restart flasher
        $wasFlashing = Test-Path $FlagFile
        Remove-Item $FlagFile -Force
        Start-Sleep -Milliseconds 800
        1..6 | ForEach-Object {
            Set-CapsLed $true;  Start-Sleep -Milliseconds 110
            Set-CapsLed $false; Start-Sleep -Milliseconds 110
        }
        if ($wasFlashing) {
            New-Item -ItemType File -Path $FlagFile -Force | Out-Null
            Start-Process -WindowStyle Hidden powershell.exe -ArgumentList @(
                '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath, 'flash')
        } else {
            Set-CapsLed $true   # stay solid: waiting for user counts as "attention on"
        }
    }
}
