# Two made-up files dragged from a real File Explorer window onto a chat's message box (the native_file_drop
# fixture: real GPUI window, OLE drop target, synthetic data) in the authorized win11 console. No tenant or account.
#Requires -Version 7.0
[CmdletBinding()]
param([switch]$Serve, [string]$Executable, [string]$RunDirectory)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
. "$PSScriptRoot/lib/windows-runner.ps1"
. "$PSScriptRoot/lib/windows-synthetic-shot.ps1"
$title = 'NGTEAMS SYNTHETIC FILE DROP'
$names = @('made-up notes.txt', 'made-up-report.pdf')

function Add-NgteamsDragInput {
    if ('NgteamsDragInput' -as [type]) { return }
    Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Threading;
public static class NgteamsDragInput {
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] struct MouseInput { public int X, Y; public uint Data, Flags, Time; public IntPtr Extra; }
    [StructLayout(LayoutKind.Sequential)] struct KeyInput { public ushort Key, Scan; public uint Flags, Time; public IntPtr Extra; public uint Pad1, Pad2; }
    [StructLayout(LayoutKind.Explicit)] struct Input { [FieldOffset(0)] public uint Type; [FieldOffset(8)] public MouseInput Mouse; [FieldOffset(8)] public KeyInput Key; }
    [DllImport("user32.dll")] static extern uint SendInput(uint count, Input[] inputs, int size);
    [DllImport("user32.dll")] static extern int GetSystemMetrics(int index);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr window, ref Point point);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
    const uint Move = 0x0001, LeftDown = 0x0002, LeftUp = 0x0004, Absolute = 0x8000, VirtualDesk = 0x4000;
    static void Send(Input input) {
        if (SendInput(1, new[] { input }, Marshal.SizeOf(typeof(Input))) != 1) throw new InvalidOperationException("SendInput refused");
    }
    static Input At(int x, int y, uint flags) {
        int left = GetSystemMetrics(76), top = GetSystemMetrics(77), width = GetSystemMetrics(78), height = GetSystemMetrics(79);
        var mouse = new MouseInput {
            X = (int)Math.Round((x - left) * 65535.0 / (width - 1)), Y = (int)Math.Round((y - top) * 65535.0 / (height - 1)),
            Flags = flags | Move | Absolute | VirtualDesk };
        return new Input { Type = 0, Mouse = mouse };
    }
    public static void MoveTo(int x, int y) { Send(At(x, y, 0)); }
    public static void Click(int x, int y, bool control) {
        if (control) Send(new Input { Type = 1, Key = new KeyInput { Key = 0x11 } });
        Send(At(x, y, 0)); Thread.Sleep(60); Send(At(x, y, LeftDown)); Thread.Sleep(60); Send(At(x, y, LeftUp));
        if (control) Send(new Input { Type = 1, Key = new KeyInput { Key = 0x11, Flags = 2 } });
        Thread.Sleep(250);
    }
    public static void Drag(int fromX, int fromY, int toX, int toY) {
        Send(At(fromX, fromY, 0)); Thread.Sleep(150);
        Send(At(fromX, fromY, LeftDown)); Thread.Sleep(150);
        const int steps = 40;
        for (int step = 1; step <= steps; step++) {
            Send(At(fromX + (toX - fromX) * step / steps, fromY + (toY - fromY) * step / steps, 0));
            Thread.Sleep(25);
        }
        Thread.Sleep(600);
        Send(At(toX + 2, toY, 0)); Thread.Sleep(600);
        Send(At(toX + 2, toY, LeftUp));
    }
}
'@
}

function Wait-NgteamsLine([string]$File, [string]$Pattern, [int]$Seconds) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    do {
        if (Test-Path -LiteralPath $File) {
            $line = Get-Content -LiteralPath $File | Where-Object { $_ -match $Pattern } | Select-Object -First 1
            if ($line) { return $line }
        }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "No line matching '$Pattern' in $File within $Seconds s."
}

function Find-NgteamsExplorerItems([string]$Folder) {
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
    $automation = [Windows.Automation.AutomationElement]
    $leaf = Split-Path $Folder -Leaf
    $explorers = New-Object Windows.Automation.PropertyCondition($automation::ClassNameProperty, 'CabinetWClass')
    $rows = New-Object Windows.Automation.PropertyCondition($automation::ControlTypeProperty,
        [Windows.Automation.ControlType]::ListItem)
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        # Windows 11 names the window "<folder> - File Explorer"; a row drops a known extension when Explorer hides them
        $window = $automation::RootElement.FindAll([Windows.Automation.TreeScope]::Children, $explorers) |
            Where-Object { $_.Current.Name -eq $leaf -or $_.Current.Name -like "$leaf - *" } | Select-Object -First 1
        if ($window) {
            $all = @($window.FindAll([Windows.Automation.TreeScope]::Descendants, $rows))
            $items = foreach ($name in $names) {
                $all | Where-Object {
                    $_.Current.Name -eq $name -or $_.Current.Name -eq [IO.Path]::GetFileNameWithoutExtension($name)
                } | Select-Object -First 1
            }
            if (@($items | Where-Object { $_ }).Count -eq $names.Count) { return @{ window = $window; items = $items } }
        }
        Start-Sleep -Milliseconds 300
    } while ([DateTime]::UtcNow -lt $deadline)
    $seen = if ($window) { ($all | ForEach-Object { $_.Current.Name }) -join ', ' } else { 'no window' }
    throw "File Explorer did not show $($names -join ', ') in $Folder (saw: $seen)."
}

# Windows a run that failed left: Explorer on a made-up folder, and its "Replace or Skip Files" question about one.
function Close-NgteamsLeftovers {
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
    $automation = [Windows.Automation.AutomationElement]
    $links = New-Object Windows.Automation.PropertyCondition($automation::NameProperty, 'Made-up files')
    foreach ($window in $automation::RootElement.FindAll([Windows.Automation.TreeScope]::Children,
            [Windows.Automation.Condition]::TrueCondition)) {
        $name = $window.Current.Name
        $ours = ($window.Current.ClassName -eq 'CabinetWClass' -and $name -like 'Made-up files*') -or
            ($name -eq 'Replace or Skip Files' -and $window.FindFirst([Windows.Automation.TreeScope]::Descendants, $links))
        if ($ours) {
            try { $window.GetCurrentPattern([Windows.Automation.WindowPattern]::Pattern).Close() } catch { }
        }
    }
}

function Get-NgteamsCentre($Element) {
    $rect = $Element.Current.BoundingRectangle
    return @([int]($rect.X + $rect.Width / 2), [int]($rect.Y + $rect.Height / 2))
}

if ($Serve) {
    $env:NGTEAMS_WINDOWS_VM_SANDBOX = 'win11'
    $env:UI_SANDBOX_DIR = $RunDirectory
    $env:NGTEAMS_NATIVE_FILE_DROP_REQUIRE = '1'
    Set-NgteamsMediaEnvironment $root
    $log = Join-Path $RunDirectory 'driver.log'
    $out = Join-Path $RunDirectory 'test.out'
    # a folder of its own, so no other run's Explorer window is taken for this one
    $folder = Join-Path $RunDirectory ('Made-up files ' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
    Close-NgteamsLeftovers
    New-Item -ItemType Directory -Path $folder | Out-Null
    foreach ($name in $names) { Set-Content -LiteralPath (Join-Path $folder $name) -Value 'A synthetic attachment, not a real document.' }
    Add-NgteamsDragInput
    [NgteamsDragInput]::SetProcessDPIAware() | Out-Null
    $process = Start-Process $Executable -NoNewWindow -PassThru `
        -RedirectStandardOutput $out -RedirectStandardError (Join-Path $RunDirectory 'test.err')
    @{ id=$process.Id; started=$process.StartTime.ToUniversalTime().Ticks } | ConvertTo-Json -Compress |
        Set-Content -LiteralPath (Join-Path $RunDirectory 'fixture-process.json')
    $explorer = $null
    try {
        $box = (Wait-NgteamsLine $out '^box ' 60).Split(' ')
        Wait-NgteamsLine $out '^ready$' 30 | Out-Null
        $process.Refresh()
        $point = New-Object NgteamsDragInput+Point
        $scale = [double]$box[3]
        $point.X = [int]([double]$box[1] * $scale); $point.Y = [int]([double]$box[2] * $scale)
        if (![NgteamsDragInput]::ClientToScreen($process.MainWindowHandle, [ref]$point)) { throw 'No client origin for the fixture window.' }
        Add-Content -LiteralPath $log -Value "message box at $($point.X),$($point.Y)"
        Start-Process explorer.exe -ArgumentList ('"' + $folder + '"')
        $found = Find-NgteamsExplorerItems $folder
        $explorer = $found.window
        # Explorer in the top-left corner, clear of the chat's message box, so the drop lands on the chat
        $place = $explorer.GetCurrentPattern([Windows.Automation.TransformPattern]::Pattern)
        $place.Move(0, 0)
        $place.Resize(520, 400)
        Start-Sleep -Milliseconds 800
        $found = Find-NgteamsExplorerItems $folder
        $first = Get-NgteamsCentre $found.items[0]
        $second = Get-NgteamsCentre $found.items[1]
        Add-Content -LiteralPath $log -Value "items at $($first -join ','), $($second -join ',')"
        [NgteamsDragInput]::Click($first[0], $first[1], $false)
        [NgteamsDragInput]::Click($second[0], $second[1], $true)
        # Explorer takes the selection before a press on it starts a drag
        Start-Sleep -Milliseconds 1500
        [NgteamsDragInput]::Drag($second[0], $second[1], $point.X, $point.Y)
        $hover = Wait-NgteamsLine $out '^hover true$' 15
        $files = Wait-NgteamsLine $out '^files ' 30
        Add-Content -LiteralPath $log -Value $files
        $staged = $files.Substring(6).Split('|') | Sort-Object
        if (Compare-Object $staged ($names | Sort-Object)) { throw "Staged '$files', expected $($names -join ', ')." }
        # the capture refuses a client another window covers
        $explorer.GetCurrentPattern([Windows.Automation.WindowPattern]::Pattern).Close()
        $explorer = $null
        [NgteamsDragInput]::SetForegroundWindow($process.MainWindowHandle) | Out-Null
        Start-Sleep -Milliseconds 800
        Save-NgteamsSyntheticShot $process $title (Join-Path $RunDirectory 'dropped.png') | Out-Null
        Set-Content -LiteralPath (Join-Path $RunDirectory 'passed') -Value 'native file drop passed'
    } catch {
        [IO.File]::WriteAllText((Join-Path $RunDirectory 'failure.txt'), $_.ToString())
        throw
    } finally {
        if ($explorer) {
            try { $explorer.GetCurrentPattern([Windows.Automation.WindowPattern]::Pattern).Close() } catch { }
        }
        $process.Refresh()
        if (!$process.HasExited) { $process.Kill($true); $process.WaitForExit() }
    }
    exit 0
}
Push-Location $root
try {
    Set-NgteamsMediaEnvironment $root
    if ($Executable) {
        if (![IO.Path]::IsPathFullyQualified($Executable) -or
            [IO.Path]::GetFileName($Executable) -notmatch '^native_file_drop-[a-f0-9]+\.exe$' -or
            !(Test-Path -LiteralPath $Executable -PathType Leaf)) { throw 'Expected an absolute native_file_drop test executable.' }
        $exe = $Executable
    } else {
        & cargo test --locked -p teams-ui --test native_file_drop --no-run --target-dir target/lab
        if ($LASTEXITCODE -ne 0) { throw 'Native file drop test did not build.' }
        $exe = (Get-ChildItem target/lab/debug/deps/native_file_drop-*.exe | Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
        if (!$exe) { throw 'Native file drop test executable missing.' }
    }
    $run = Join-Path $root ('target/windows-file-drop-' + [Guid]::NewGuid().ToString('N'))
    $runner = Get-NgteamsRunner $root
    & $runner prepare --directory $run
    if ($LASTEXITCODE -ne 0) { throw 'Cannot prepare private console evidence directory.' }
    $taskName = 'ngteams-file-drop-synthetic-' + [Guid]::NewGuid().ToString('N')
    # the console task starts with the user's environment: hand it the GStreamer this build found
    $gst = Get-NgteamsGStreamerRoot $root
    $media = if ($gst) { '$env:GSTREAMER_1_0_ROOT_MSVC_X86_64 = ' + (ConvertTo-NgteamsLiteral $gst) + '; ' } else { '' }
    $command = $media + '& ' + (ConvertTo-NgteamsLiteral $PSCommandPath) + ' -Serve -Executable ' +
        (ConvertTo-NgteamsLiteral $exe) + ' -RunDirectory ' + (ConvertTo-NgteamsLiteral $run) +
        ' *> ' + (ConvertTo-NgteamsLiteral (Join-Path $run 'console.log'))
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
    $action = New-ScheduledTaskAction -Execute (Get-Command pwsh -CommandType Application | Select-Object -First 1).Source `
        -Argument "-NoProfile -NonInteractive -EncodedCommand $encoded" -WorkingDirectory $root
    $principal = New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) `
        -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Seconds 240)
    try {
        Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings | Out-Null
        Start-ScheduledTask -TaskName $taskName
        $deadline = [DateTime]::UtcNow.AddSeconds(230)
        do {
            Start-Sleep -Milliseconds 300
            if (Test-Path -LiteralPath (Join-Path $run 'passed')) { break }
            $task = Get-ScheduledTask -TaskName $taskName
            if ($task.State -ne 'Running' -or [DateTime]::UtcNow -ge $deadline) {
                throw "Native file drop did not pass; evidence: $run"
            }
        } while ($true)
        Write-Output "Native file drop passed; evidence: $run"
    } finally {
        Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
        $owned = Join-Path $run 'fixture-process.json'
        if (Test-Path -LiteralPath $owned) {
            $record = Get-Content -LiteralPath $owned -Raw | ConvertFrom-Json
            $child = Get-Process -Id $record.id -ErrorAction SilentlyContinue
            if ($child -and $child.Path -eq $exe -and $child.StartTime.ToUniversalTime().Ticks -eq $record.started) {
                $child.Kill($true); $child.WaitForExit()
            }
        }
    }
} finally { Pop-Location }
