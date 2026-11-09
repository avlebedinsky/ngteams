# Windows counterpart of run-lab.sh. The explicitly authorized win11 VM console is the UI sandbox.
# Build only: ./scripts/run-lab.ps1 -BuildOnly
# Start: ./scripts/run-lab.ps1 a -TimeoutSeconds 1800 (after the agent's lab-contact check).
# Provision the alias's accounts.toml with exactly one lab account first; credentials never go through this script.
# Stop/status: ./scripts/run-lab.ps1 a -Stop / -Status. Logs and account data are retained.
#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Position=0)][ValidatePattern('^[A-Za-z0-9_-]+$')][string]$Alias,
    [switch]$BuildOnly,
    [switch]$Stop,
    [switch]$Status,
    [ValidateRange(1,86400)][int]$TimeoutSeconds = 1800,
    [switch]$Serve
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/lib/windows-runner.ps1"
$root = Split-Path $PSScriptRoot
if ($BuildOnly) {
    Build-NgteamsLab $root $TimeoutSeconds
    exit $LASTEXITCODE
}
if (!$Alias) { throw 'A lab alias is required (or use -BuildOnly).' }
if (!$Stop -and !$Status -and !$Serve) {
    Build-NgteamsLab $root $TimeoutSeconds
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
$directory = Join-Path $env:LOCALAPPDATA "ngteams-lab/$Alias"
Invoke-NgteamsRunner -Root $root -Directory $directory -ScopeName Alias -Scope $Alias `
    -App (Join-Path $root 'target/lab/debug/ngteams.exe') -TimeoutSeconds $TimeoutSeconds `
    -Stop:$Stop -Status:$Status -Serve:$Serve -Script $PSCommandPath
