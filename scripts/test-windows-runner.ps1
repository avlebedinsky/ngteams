# Offline native runner smoke: a tiny synthetic executable, no GUI, network, credentials, or ngteams build.
# Run in the authorized Windows VM. Pass -Runner to an instrumented helper to measure its CLI/console paths.
#Requires -Version 7.0
[CmdletBinding()]
param(
    [string]$Runner = (Join-Path (Split-Path $PSScriptRoot) 'target/runner/debug/teams-runner.exe'),
    [switch]$Console,
    [switch]$SkipConsole,
    [string]$Fixture,
    [string]$RunDirectory,
    [string]$Profile
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/lib/windows-runner.ps1"
if ($Console) {
    if ($Profile) { $env:LLVM_PROFILE_FILE = $Profile }
    $env:NGTEAMS_WINDOWS_VM_SANDBOX = 'win11'
    $env:UI_SANDBOX_DIR = $RunDirectory
    & $Runner run --app $Fixture --directory $RunDirectory --account synthetic@example.test --timeout 1
    if ($LASTEXITCODE -ne 124) { throw "Expected deadline exit 124, got $LASTEXITCODE" }
    if (Test-Path -LiteralPath (Join-Path $RunDirectory 'snapshot')) { throw 'Snapshot was not removed.' }
    # The synthetic app-exits case reports its original exit code rather than converting it to success.
    & $Runner run --app $Fixture --directory $RunDirectory --alias app-exits --timeout 5
    if ($LASTEXITCODE -ne 7) { throw "Expected app exit 7, got $LASTEXITCODE" }
    Set-Content -LiteralPath (Join-Path $RunDirectory 'passed') -Value 'synthetic console cases passed'
    exit 0
}
$runnerPath = (Resolve-Path -LiteralPath $Runner).Path
$root = Split-Path $PSScriptRoot
$runDir = Join-Path $root ('target/windows-runner-smoke-' + [Guid]::NewGuid().ToString('N'))
& $runnerPath prepare --directory $runDir
if ($LASTEXITCODE -ne 0) { throw 'Prepare failed.' }
$fixturePath = Join-Path $runDir 'synthetic-app.exe'
& rustc (Join-Path $root 'crates/teams-runner/tests/support/app.rs') -o $fixturePath
if ($LASTEXITCODE -ne 0) { throw 'Synthetic fixture did not build.' }
# Build command dispatch uses the synthetic executable in place of Cargo. It does not compile a client.
try {
    $previousLab = $env:TEAMS_LAB_BUILD
    $env:TEAMS_LAB_BUILD = '1'
    & $runnerPath build --cargo $fixturePath --timeout 5
    if ($LASTEXITCODE -ne 0) { throw 'Synthetic build dispatch failed.' }
} finally { $env:TEAMS_LAB_BUILD = $previousLab }
& $runnerPath build --cargo $fixturePath --timeout 5
if ($LASTEXITCODE -ne 1) { throw 'An unmarked build must be refused.' }
# From SSH, a UI launch must fail before spawning even the synthetic app.
& $runnerPath run --app $fixturePath --directory $runDir --alias a --timeout 1
if ($LASTEXITCODE -ne 1) { throw 'A non-console GUI launch must be refused.' }
if ($SkipConsole) {
    Write-Output "Synthetic CLI smoke passed; evidence: $runDir"
    exit 0
}
$taskName = 'ngteams-runner-synthetic-' + [Guid]::NewGuid().ToString('N')
$command = '& ' + (ConvertTo-NgteamsLiteral $PSCommandPath) + ' -Console -Runner ' +
    (ConvertTo-NgteamsLiteral $runnerPath) + ' -Fixture ' + (ConvertTo-NgteamsLiteral $fixturePath) +
    ' -RunDirectory ' + (ConvertTo-NgteamsLiteral $runDir)
if ($env:LLVM_PROFILE_FILE) { $command += ' -Profile ' + (ConvertTo-NgteamsLiteral $env:LLVM_PROFILE_FILE) }
$command += ' *> ' + (ConvertTo-NgteamsLiteral (Join-Path $runDir 'console.log'))
$encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
$action = New-ScheduledTaskAction -Execute (Get-Command pwsh -CommandType Application).Source `
    -Argument "-NoProfile -NonInteractive -EncodedCommand $encoded" -WorkingDirectory $root
$principal = New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) `
    -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Seconds 60)
try {
    Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings | Out-Null
    Start-ScheduledTask -TaskName $taskName
    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    do {
        Start-Sleep -Milliseconds 200
        if (Test-Path -LiteralPath (Join-Path $runDir 'passed')) { break }
        if ([DateTime]::UtcNow -ge $deadline) { $info = Get-ScheduledTaskInfo -TaskName $taskName
            throw "Console smoke deadline; task result $($info.LastTaskResult); evidence in $runDir" }
    } while ($true)
    $task = Get-ScheduledTask -TaskName $taskName
    while ($task.State -eq 'Running' -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 100
        $task = Get-ScheduledTask -TaskName $taskName
    }
    if ($task.State -eq 'Running') { throw "Console cleanup did not finish: $runDir" }
    Write-Output "Synthetic runner smoke passed; evidence: $runDir"
} finally {
    Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
}
exit 0
