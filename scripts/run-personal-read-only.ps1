# Windows counterpart of run-personal-read-only.sh. Runs only an already installed executable, with a get-only
# credential broker, a disposable snapshot, and the explicit win11 VM console sandbox. Does not build/install it.
# ./scripts/run-personal-read-only.ps1 user@example.test -App C:/path/to/ngteams/bin/ngteams.exe
# ./scripts/run-personal-read-only.ps1 -Status / -Stop. Snapshot is deleted after orderly stop, logs retained.
#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Position=0)][string]$Account,
    [string]$App = $env:TEAMS_BIN,
    [switch]$Stop,
    [switch]$Status,
    [ValidateRange(1,86400)][int]$TimeoutSeconds = 1800,
    [switch]$Serve
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/lib/windows-runner.ps1"
$directory = Join-Path $env:LOCALAPPDATA 'ngteams-read-only/run'
Invoke-NgteamsRunner -Root (Split-Path $PSScriptRoot) -Directory $directory -ScopeName Account -Scope $Account `
    -App $App -TimeoutSeconds $TimeoutSeconds -Stop:$Stop -Status:$Status -Serve:$Serve -Script $PSCommandPath
