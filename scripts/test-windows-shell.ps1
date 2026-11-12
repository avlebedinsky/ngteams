# Offline real GPUI Desktop lifecycle and native shell over synthetic accounts. No tenant or media device.
#Requires -Version 7.0
[CmdletBinding()]
param([switch]$Serve, [string]$Executable, [string]$RunDirectory)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
. "$PSScriptRoot/lib/windows-runner.ps1"
. "$PSScriptRoot/lib/windows-synthetic-shot.ps1"
. "$PSScriptRoot/lib/windows-shell-fixture.ps1"
. "$PSScriptRoot/lib/windows-toast-fixture.ps1"
if ($Serve) {
    $env:NGTEAMS_WINDOWS_VM_SANDBOX = 'win11'
    $env:UI_SANDBOX_DIR = $RunDirectory
    $env:NGTEAMS_NATIVE_SHELL_REQUIRE = '1'
    Set-NgteamsMediaEnvironment $root
    $process = Start-Process $Executable -NoNewWindow -PassThru `
        -RedirectStandardOutput (Join-Path $RunDirectory 'test.out') -RedirectStandardError (Join-Path $RunDirectory 'test.err')
    @{ id=$process.Id; started=$process.StartTime.ToUniversalTime().Ticks } | ConvertTo-Json -Compress |
        Set-Content -LiteralPath (Join-Path $RunDirectory 'fixture-process.json')
    try {
        Invoke-NgteamsSyntheticShell $process $RunDirectory
        if (!$process.WaitForExit(5000)) { throw 'Native shell did not exit after its final checkpoint.' }
        if ($process.ExitCode -ne 0) { throw "Native shell failed ($($process.ExitCode)); evidence: $RunDirectory" }
        Set-Content -LiteralPath (Join-Path $RunDirectory 'passed') -Value 'native shell contract passed'
    } catch {
        [IO.File]::WriteAllText((Join-Path $RunDirectory 'failure.txt'), $_.ToString())
        throw
    } finally {
        [IO.File]::AppendAllText((Join-Path $RunDirectory 'driver.log'), 'cleanup begin'+[Environment]::NewLine)
        $process.Refresh()
        if (!$process.HasExited) { $process.Kill($true); $process.WaitForExit() }
        [IO.File]::AppendAllText((Join-Path $RunDirectory 'driver.log'), 'process stopped'+[Environment]::NewLine)
        Remove-NgteamsSyntheticShellIdentity $process.Id
        [IO.File]::AppendAllText((Join-Path $RunDirectory 'driver.log'), 'identity removed'+[Environment]::NewLine)
    }
    exit 0
}
Push-Location $root
try {
    Set-NgteamsMediaEnvironment $root
    if ($Executable) {
        if (![IO.Path]::IsPathFullyQualified($Executable) -or
            [IO.Path]::GetFileName($Executable) -notmatch '^native_shell-[a-f0-9]+\.exe$' -or
            !(Test-Path -LiteralPath $Executable -PathType Leaf)) { throw 'Expected an absolute native_shell test executable.' }
        $exe = $Executable
    } else {
        & cargo test --locked -p teams-ui --test native_shell --no-run --target-dir target/lab
        if ($LASTEXITCODE -ne 0) { throw 'Native shell test did not build.' }
        $exe = (Get-ChildItem target/lab/debug/deps/native_shell-*.exe | Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
        if (!$exe) { throw 'Native shell test executable missing.' }
    }
    $run = Join-Path $root ('target/windows-shell-smoke-' + [Guid]::NewGuid().ToString('N'))
    $runner = Get-NgteamsRunner $root
    & $runner prepare --directory $run
    if ($LASTEXITCODE -ne 0) { throw 'Cannot prepare private console evidence directory.' }
    $taskName = 'ngteams-shell-synthetic-' + [Guid]::NewGuid().ToString('N')
    $command = '& ' + (ConvertTo-NgteamsLiteral $PSCommandPath) + ' -Serve -Executable ' +
        (ConvertTo-NgteamsLiteral $exe) + ' -RunDirectory ' + (ConvertTo-NgteamsLiteral $run) +
        ' *> ' + (ConvertTo-NgteamsLiteral (Join-Path $run 'console.log'))
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
    $action = New-ScheduledTaskAction -Execute (Get-Command pwsh -CommandType Application | Select-Object -First 1).Source `
        -Argument "-NoProfile -NonInteractive -EncodedCommand $encoded" -WorkingDirectory $root
    $principal = New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) `
        -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Seconds 180)
    try {
        Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings | Out-Null
        Start-ScheduledTask -TaskName $taskName
        $deadline = [DateTime]::UtcNow.AddSeconds(170)
        do {
            Start-Sleep -Milliseconds 200
            if (Test-Path -LiteralPath (Join-Path $run 'passed')) { break }
            $task = Get-ScheduledTask -TaskName $taskName
            if ($task.State -ne 'Running' -or [DateTime]::UtcNow -ge $deadline) {
                throw "Native shell smoke did not pass; evidence: $run"
            }
        } while ($true)
        Write-Output "Native shell smoke passed; evidence: $run"
    } finally {
        Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
        foreach ($recordName in @('driver-process.json','fixture-process.json')) {
            $owned = Join-Path $run $recordName
            if (Test-Path -LiteralPath $owned) {
                $record = Get-Content -LiteralPath $owned -Raw | ConvertFrom-Json
                $child = Get-Process -Id $record.id -ErrorAction SilentlyContinue
                $same = $child -and $child.Path -eq $exe -and $child.StartTime.ToUniversalTime().Ticks -eq $record.started
                if (!$child -or $same) {
                    if ($same) { $child.Kill($true); $child.WaitForExit() }
                    if ($recordName -eq 'fixture-process.json') { Remove-NgteamsSyntheticShellIdentity $record.id }
                }
            }
        }
    }
} finally { Pop-Location }
