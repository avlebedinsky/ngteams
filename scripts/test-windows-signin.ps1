# Offline WebView2 channel/guard contract. No ngteams build, tenant, account or credentials.
#Requires -Version 7.0
[CmdletBinding()]
param([switch]$Serve, [switch]$HoldForShot, [string]$Executable, [string]$RunDirectory)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
. "$PSScriptRoot/lib/windows-runner.ps1"
if ($Serve) {
    $env:NGTEAMS_WINDOWS_VM_SANDBOX = 'win11'
    $env:UI_SANDBOX_DIR = $RunDirectory
    $env:NGTEAMS_NATIVE_SIGNIN_REQUIRE = '1'
    if ($HoldForShot) { $env:NGTEAMS_SIGNIN_TEST_HOLD = '1' }
    $process = Start-Process $Executable -NoNewWindow -PassThru `
        -RedirectStandardOutput (Join-Path $RunDirectory 'test.out') -RedirectStandardError (Join-Path $RunDirectory 'test.err')
    if (!$process.WaitForExit(90000)) { $process.Kill($true); throw 'Native sign-in test deadline expired.' }
    if ($process.ExitCode -ne 0) { throw "Native sign-in test failed ($($process.ExitCode)); evidence: $RunDirectory" }
    Set-Content -LiteralPath (Join-Path $RunDirectory 'passed') -Value 'native sign-in contract passed'
    exit 0
}
Push-Location $root
try {
    & cargo test --locked -p teams-signin --test native_window --no-run --target-dir target/lab
    if ($LASTEXITCODE -ne 0) { throw 'Native sign-in test did not build.' }
    $exe = (Get-ChildItem target/lab/debug/deps/native_window-*.exe | Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
    if (!$exe) { throw 'Native sign-in test executable missing.' }
    $run = Join-Path $root ('target/windows-signin-smoke-' + [Guid]::NewGuid().ToString('N'))
    $runner = Get-NgteamsRunner $root
    & $runner prepare --directory $run
    if ($LASTEXITCODE -ne 0) { throw 'Cannot prepare private console evidence directory.' }
    $taskName = 'ngteams-signin-synthetic-' + [Guid]::NewGuid().ToString('N')
    $command = '& ' + (ConvertTo-NgteamsLiteral $PSCommandPath) + ' -Serve -Executable ' +
        (ConvertTo-NgteamsLiteral $exe) + ' -RunDirectory ' + (ConvertTo-NgteamsLiteral $run) +
        ' *> ' + (ConvertTo-NgteamsLiteral (Join-Path $run 'console.log'))
    if ($HoldForShot) { $command = '$env:NGTEAMS_SIGNIN_TEST_HOLD = ''1''; ' + $command }
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
                throw "Native sign-in smoke did not pass; evidence: $run"
            }
        } while ($true)
        Write-Output "Native sign-in smoke passed; evidence: $run"
    } finally {
        Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    }
} finally { Pop-Location }
