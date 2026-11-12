# Install only the hash-pinned GStreamer development SDK; CI already supplies MSVC, CMake, Python and Git.
#Requires -Version 7.0
[CmdletBinding()]
param([string]$Destination = (Join-Path (Split-Path $PSScriptRoot) 'windows-deps/gstreamer'))
$ErrorActionPreference = 'Stop'
$pins = Get-Content "$PSScriptRoot/../packaging/windows/media-pins.json" -Raw | ConvertFrom-Json
$Destination = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Destination)
if (Test-Path -LiteralPath $Destination) { throw 'SDK destination exists; use a clean destination.' }
$installer = Join-Path ([IO.Path]::GetTempPath()) ('ngteams-gstreamer-' + [Guid]::NewGuid().ToString('N') + '.exe')
try {
    Invoke-WebRequest $pins.gstreamer_url -OutFile $installer -TimeoutSec 600
    if ((Get-FileHash $installer -Algorithm SHA256).Hash -ine $pins.gstreamer_sha256) { throw 'GStreamer installer hash mismatch.' }
    $process = Start-Process $installer -ArgumentList @('/CURRENTUSER', '/VERYSILENT', '/NORESTART', '/TYPE=devel', ('/DIR="' + $Destination + '"')) -PassThru
    if (!$process.WaitForExit(600000)) { $process.Kill($true); throw 'GStreamer installation deadline expired.' }
    if ($process.ExitCode -ne 0) { throw "GStreamer installer failed ($($process.ExitCode))." }
    if (!(Test-Path "$Destination/bin/pkg-config.exe")) { throw 'GStreamer development files are missing.' }
} finally { Remove-Item -LiteralPath $installer -Force -ErrorAction SilentlyContinue }
