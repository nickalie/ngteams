# Offline native Windows toasts/tray/taskbar contract. No account, tenant, microphone or camera.
#Requires -Version 7.0
[CmdletBinding()]
param([switch]$Serve, [switch]$HoldForShot, [string]$Executable, [string]$RunDirectory)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
. "$PSScriptRoot/lib/windows-runner.ps1"
. "$PSScriptRoot/lib/windows-toast-fixture.ps1"
if ($Serve) {
    $env:NGTEAMS_WINDOWS_VM_SANDBOX = 'win11'
    $env:UI_SANDBOX_DIR = $RunDirectory
    $env:NGTEAMS_NATIVE_DESKTOP_REQUIRE = '1'
    Set-NgteamsMediaEnvironment $root
    if ($HoldForShot) { $env:NGTEAMS_NATIVE_DESKTOP_HOLD = '1' }
    $process = Start-Process $Executable -NoNewWindow -PassThru `
        -RedirectStandardOutput (Join-Path $RunDirectory 'test.out') -RedirectStandardError (Join-Path $RunDirectory 'test.err')
    @{ id=$process.Id; started=$process.StartTime.ToUniversalTime().Ticks } | ConvertTo-Json -Compress |
        Set-Content -LiteralPath (Join-Path $RunDirectory 'fixture-process.json')
    $deadline=[DateTime]::UtcNow.AddSeconds(85)
    try {
        foreach ($action in @('default','answer','decline','join')) {
            $captured=$false
            do {
                $process.Refresh()
                if ($process.HasExited) { break }
                $captured=Save-NgteamsSyntheticToast $RunDirectory $action
                if (!$captured) { Start-Sleep -Milliseconds 100 }
            } while (!$captured -and [DateTime]::UtcNow -lt $deadline)
            if (!$captured) { throw "Synthetic toast action was not available: $action" }
        }
        $remaining=[Math]::Max(1,[int]($deadline-[DateTime]::UtcNow).TotalMilliseconds)
        if (!$process.WaitForExit($remaining)) { throw 'Native desktop test deadline expired.' }
        if ($process.ExitCode -ne 0) { throw "Native desktop test failed ($($process.ExitCode)); evidence: $RunDirectory" }
        Set-Content -LiteralPath (Join-Path $RunDirectory 'passed') -Value 'native desktop contract passed'
    } finally {
        $process.Refresh()
        if (!$process.HasExited) { $process.Kill($true); $process.WaitForExit() }
        Remove-NgteamsSyntheticShellIdentity $process.Id
    }

    exit 0
}
Push-Location $root
try {
    Set-NgteamsMediaEnvironment $root
    & cargo test --locked -p teams-ui --test native_desktop --no-run --target-dir target/lab
    if ($LASTEXITCODE -ne 0) { throw 'Native desktop test did not build.' }
    $exe = (Get-ChildItem target/lab/debug/deps/native_desktop-*.exe | Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
    if (!$exe) { throw 'Native desktop test executable missing.' }
    $run = Join-Path $root ('target/windows-desktop-smoke-' + [Guid]::NewGuid().ToString('N'))
    $runner = Get-NgteamsRunner $root
    & $runner prepare --directory $run
    if ($LASTEXITCODE -ne 0) { throw 'Cannot prepare private console evidence directory.' }
    $taskName = 'ngteams-desktop-synthetic-' + [Guid]::NewGuid().ToString('N')
    $command = '& ' + (ConvertTo-NgteamsLiteral $PSCommandPath) + ' -Serve -Executable ' +
        (ConvertTo-NgteamsLiteral $exe) + ' -RunDirectory ' + (ConvertTo-NgteamsLiteral $run) +
        ' *> ' + (ConvertTo-NgteamsLiteral (Join-Path $run 'console.log'))
    if ($HoldForShot) { $command = '$env:NGTEAMS_NATIVE_DESKTOP_HOLD = ''1''; ' + $command }
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
    $action = New-ScheduledTaskAction -Execute (Get-Command pwsh -CommandType Application | Select-Object -First 1).Source `
        -Argument "-NoProfile -MTA -NonInteractive -EncodedCommand $encoded" -WorkingDirectory $root
    $principal = New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) `
        -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Seconds 120)
    try {
        Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings | Out-Null
        Start-ScheduledTask -TaskName $taskName
        $deadline = [DateTime]::UtcNow.AddSeconds(110)
        do {
            Start-Sleep -Milliseconds 200
            if (Test-Path -LiteralPath (Join-Path $run 'passed')) { break }
            $task = Get-ScheduledTask -TaskName $taskName
            if ($task.State -ne 'Running' -or [DateTime]::UtcNow -ge $deadline) {
                throw "Native desktop smoke did not pass; evidence: $run"
            }
        } while ($true)
        Write-Output "Native desktop smoke passed; evidence: $run"
    } finally {
        Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
        $owned = Join-Path $run 'fixture-process.json'
        if (Test-Path -LiteralPath $owned) {
            $record=Get-Content -LiteralPath $owned -Raw | ConvertFrom-Json
            $child=Get-Process -Id $record.id -ErrorAction SilentlyContinue
            $same=$child -and $child.Path -eq $exe -and $child.StartTime.ToUniversalTime().Ticks -eq $record.started
            if (!$child -or $same) {
                if ($same) { $child.Kill($true); $child.WaitForExit() }
                Remove-NgteamsSyntheticShellIdentity $record.id
            }
        }

    }
} finally { Pop-Location }
