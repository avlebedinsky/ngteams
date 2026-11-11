# Offline native GPUI video rendering/atlas contract. No account, tenant or media device.
#Requires -Version 7.0
[CmdletBinding()]
param([switch]$Serve, [switch]$HoldForShot, [string]$Executable, [string]$RunDirectory)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
. "$PSScriptRoot/lib/windows-runner.ps1"
. "$PSScriptRoot/lib/windows-synthetic-shot.ps1"
if ($Serve) {
    $env:NGTEAMS_WINDOWS_VM_SANDBOX = 'win11'
    $env:UI_SANDBOX_DIR = $RunDirectory
    $env:NGTEAMS_NATIVE_VIDEO_REQUIRE = '1'
    Set-NgteamsMediaEnvironment $root
    if ($HoldForShot) { $env:NGTEAMS_NATIVE_VIDEO_HOLD = '1' }
    $process = Start-Process $Executable -NoNewWindow -PassThru `
        -RedirectStandardOutput (Join-Path $RunDirectory 'test.out') -RedirectStandardError (Join-Path $RunDirectory 'test.err')
    try {
        $deadline=[DateTime]::UtcNow.AddSeconds(90)
        if ($env:NGTEAMS_NATIVE_VIDEO_HOLD -eq '1') {
            $shot=$false
            do {
                Start-Sleep -Milliseconds 100
                $process.Refresh()
                if ($process.HasExited) { break }
                $ready=Select-String -LiteralPath (Join-Path $RunDirectory 'test.out') -Pattern 'synthetic video ready' -Quiet
                if ($ready) {
                    $shot=Save-NgteamsSyntheticShot $process 'NGTEAMS SYNTHETIC VIDEO' (Join-Path $RunDirectory 'frame.png')
                }
            } while (!$shot -and [DateTime]::UtcNow -lt $deadline)
            if (!$shot) { throw 'Native video did not capture its requested foreground client-area screenshot.' }
        }
        $remaining=[Math]::Max(1,[int]($deadline-[DateTime]::UtcNow).TotalMilliseconds)
        if (!$process.WaitForExit($remaining)) { throw 'Native video test deadline expired.' }
        if ($process.ExitCode -ne 0) { throw "Native video test failed ($($process.ExitCode)); evidence: $RunDirectory" }
        Set-Content -LiteralPath (Join-Path $RunDirectory 'passed') -Value 'native video contract passed'
    } finally {
        $process.Refresh()
        if (!$process.HasExited) { $process.Kill($true); $process.WaitForExit() }
        $process.Dispose()
    }
    exit 0
}
Push-Location $root
try {
    Set-NgteamsMediaEnvironment $root
    & cargo test --locked -p teams-ui --test native_video --no-run --target-dir target/lab
    if ($LASTEXITCODE -ne 0) { throw 'Native video test did not build.' }
    $exe = (Get-ChildItem target/lab/debug/deps/native_video-*.exe | Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
    if (!$exe) { throw 'Native video test executable missing.' }
    $run = Join-Path $root ('target/windows-video-smoke-' + [Guid]::NewGuid().ToString('N'))
    $runner = Get-NgteamsRunner $root
    & $runner prepare --directory $run
    if ($LASTEXITCODE -ne 0) { throw 'Cannot prepare private console evidence directory.' }
    $taskName = 'ngteams-video-synthetic-' + [Guid]::NewGuid().ToString('N')
    $command = '& ' + (ConvertTo-NgteamsLiteral $PSCommandPath) + ' -Serve -Executable ' +
        (ConvertTo-NgteamsLiteral $exe) + ' -RunDirectory ' + (ConvertTo-NgteamsLiteral $run) +
        ' *> ' + (ConvertTo-NgteamsLiteral (Join-Path $run 'console.log'))
    if ($HoldForShot) { $command = '$env:NGTEAMS_NATIVE_VIDEO_HOLD = ''1''; ' + $command }
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
    $action = New-ScheduledTaskAction -Execute (Get-Command pwsh -CommandType Application | Select-Object -First 1).Source `
        -Argument "-NoProfile -NonInteractive -EncodedCommand $encoded" -WorkingDirectory $root
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
                throw "Native video smoke did not pass; evidence: $run"
            }
        } while ($true)
        Write-Output "Native video smoke passed; evidence: $run"
    } finally {
        Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    }
} finally { Pop-Location }
