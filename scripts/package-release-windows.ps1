# Package prebuilt binaries. The release workflow builds the install-marked app; agents use lab binaries only.
#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Binaries,
    [Parameter(Mandatory)][ValidatePattern('^\d+\.\d+\.\d+\.\d+$')][string]$Version,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-f]{40}$')][string]$Commit,
    [string]$Output = 'dist',
    [string]$Runtime
)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
$Binaries = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Binaries)
$Output = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Output)
# rust-cache prunes non-Cargo directories under target before saving them. Keep generated media outside that
# tree, and use a fresh path so updated SDK/plugin pins can never reuse a stale local runtime either.
$ownsRuntime = !$Runtime
if ($ownsRuntime) { $Runtime = Join-Path ([IO.Path]::GetTempPath()) ('ngteams-release-media-' + [Guid]::NewGuid().ToString('N')) }
try {
    $programs = @('ngteams.exe', 'ngteams-webview.exe', 'ngteamsctl.exe') | ForEach-Object { Join-Path $Binaries $_ }
    & "$PSScriptRoot/package-windows-media.ps1" -Output $Runtime -Binary $programs
    $notices = Join-Path $root ('target/windows-release-notices-' + [Guid]::NewGuid().ToString('N'))
    & python "$PSScriptRoot/windows-release-notices.py" --target $Binaries $notices
    if ($LASTEXITCODE -ne 0) { throw 'Windows dependency notices could not be collected.' }
    $stage = Join-Path $Output ('stage-' + $Version)
    $archive = Join-Path $Output "ngteams-$Version-windows-x86_64.zip"
    & python "$PSScriptRoot/windows-release.py" stage --binaries $Binaries --runtime $Runtime `
        --version $Version --commit $Commit --output $stage --archive $archive --notices $notices
    if ($LASTEXITCODE -ne 0) { throw 'Windows release packaging failed.' }
    # Only the zip is a public asset. Keep staging outside dist in CI, or remove it after successful archive creation.
    Remove-Item -LiteralPath $stage, $notices -Recurse -Force
    Write-Output "Windows archive: $archive"
} finally {
    if ($ownsRuntime -and (Test-Path -LiteralPath $Runtime)) { Remove-Item -LiteralPath $Runtime -Recurse -Force }
}
