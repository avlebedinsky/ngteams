# Windows counterpart of proto-lab browser/login/auth. Passwords are read only by the dedicated login state machine
# from the protected %LOCALAPPDATA%/teams-lab/accounts.json file. No password/token argument is accepted.
# Copy only the required lab accounts as a file over scp after preparing that directory with teams-runner prepare.
# Remove that password file when validation finishes. All tokens remain in the VM's ngteams-lab credential namespace.
#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('Browser','Login','Auth','Refresh','Reach')][string]$Command,
    [ValidateSet('a','b','c')][string]$Alias = 'a',
    [ValidatePattern('^[A-Za-z0-9_-]{1,64}$')][string]$NativeAlias,
    [ValidateRange(1,900)][int]$TimeoutSeconds = 300,
    [string[]]$Chat = @(),
    [switch]$Serve
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
. "$PSScriptRoot/lib/windows-runner.ps1"
$root = Split-Path $PSScriptRoot
$directory = Join-Path $env:LOCALAPPDATA 'teams-lab'
$binary = Join-Path $root 'target/lab/debug/proto-lab.exe'
if ($Serve) {
    $env:NGTEAMS_WINDOWS_VM_SANDBOX = 'win11'
    $env:UI_SANDBOX_DIR = $directory
    $arguments = @($Command.ToLowerInvariant())
    if ($Command -ne 'Browser') { $arguments += $Alias }
    if ($Command -in @('Login','Auth')) { $arguments += @('--timeout', $TimeoutSeconds) }
    if ($Command -ne 'Browser' -and $NativeAlias) { $arguments += @('--native-alias', $NativeAlias) }
    foreach ($id in $Chat) { $arguments += @('--chat', $id) }
    # Fixed stage/status output only; never a transcription of CDP or a shell carrying credential values.
    & $binary @arguments *> (Join-Path $directory 'provision.log')
    exit $LASTEXITCODE
}
$runner = Get-NgteamsRunner $root
& $runner prepare --directory $directory
if ($LASTEXITCODE -ne 0) { throw 'Cannot prepare private lab directory.' }
$previousIncremental = $env:CARGO_INCREMENTAL
Push-Location $root
try {
    $env:CARGO_INCREMENTAL = '0'
    $build = Start-Process (Get-Command cargo -CommandType Application).Source -NoNewWindow -PassThru `
        -ArgumentList @('build','--locked','-p','proto-lab','--bin','proto-lab','--target-dir','target/lab','-j1')
    $buildDeadline = [DateTime]::UtcNow.AddMinutes(30)
    while (!$build.WaitForExit(250)) {
        if ((Get-PSDrive C).Free -lt 1.5GB) { $build.Kill($true); throw 'Guest disk space guard reached during build.' }
        if ([DateTime]::UtcNow -gt $buildDeadline) { $build.Kill($true); throw 'Lab tool build deadline expired.' }
    }
    if ($build.ExitCode -ne 0) { throw "Lab tool build failed ($($build.ExitCode))." }
} finally { Pop-Location; $env:CARGO_INCREMENTAL = $previousIncremental }
$taskName = 'ngteams-lab-auth-' + [Guid]::NewGuid().ToString('N')
$code = '& ' + (ConvertTo-NgteamsLiteral $PSCommandPath) + ' -Serve -Command ' + $Command +
    ' -Alias ' + $Alias + ' -TimeoutSeconds ' + $TimeoutSeconds
if ($NativeAlias) { $code += ' -NativeAlias ' + (ConvertTo-NgteamsLiteral $NativeAlias) }
if ($Chat.Count -gt 0) { $code += ' -Chat @(' + (($Chat | ForEach-Object { ConvertTo-NgteamsLiteral $_ }) -join ',') + ')' }
$encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code))
$action = New-ScheduledTaskAction -Execute (Get-Command pwsh -CommandType Application).Source `
    -Argument "-NoProfile -NonInteractive -EncodedCommand $encoded" -WorkingDirectory $root
$principal = New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) `
    -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Seconds ($TimeoutSeconds + 60))
try {
    Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings | Out-Null
    Start-ScheduledTask -TaskName $taskName
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds + 60)
    do {
        Start-Sleep -Milliseconds 500
        $task = Get-ScheduledTask -TaskName $taskName
        $info = Get-ScheduledTaskInfo -TaskName $taskName
        if ((Get-PSDrive C).Free -lt 1.5GB) { throw 'Guest disk space guard reached.' }
        if ([DateTime]::UtcNow -gt $deadline) { throw 'Lab provisioning deadline expired.' }
    } while ($task.State -eq 'Running' -or $info.LastRunTime.Year -lt 2000)
    if ($info.LastTaskResult -ne 0) { throw "Lab provisioning failed ($($info.LastTaskResult)); inspect the private provision.log." }
    Get-Content (Join-Path $directory 'provision.log')
} finally {
    $task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    if ($task -and $task.State -eq 'Running') { Stop-ScheduledTask -TaskName $taskName }
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
}
