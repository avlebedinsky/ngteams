# Assemble/check the same private media runtime used by lab launches and Windows release packaging.
# No application is built or launched here. Run after installing the pinned SDK and llvm-tools-preview.
#Requires -Version 7.0
[CmdletBinding()]
param([string]$Sdk, [string]$Output, [string[]]$Binary = @(), [switch]$Plan)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
. "$PSScriptRoot/lib/windows-runner.ps1"
if (!$Output) { $Output = Join-Path $root 'target/windows-media-runtime' }
$python = (Get-Command python -CommandType Application | Select-Object -First 1).Source
$closure = Join-Path $PSScriptRoot 'windows-media-closure.py'
$extra = @($Binary | ForEach-Object { '--binary'; $_ })
if (Test-Path -LiteralPath $Output) {
    & $python $closure check $Output @extra
    if ($LASTEXITCODE -ne 0) { throw 'Existing media runtime failed verification; retain it for diagnosis and package to a new output.' }
    return
}
if (!$Sdk) { $Sdk = Get-NgteamsGStreamerRoot $root }
if (!$Sdk) { throw 'GStreamer development SDK not found.' }
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
$vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (!$vs) { throw 'Visual Studio C++ Build Tools not found (x64 CRT redistribution inputs are required).' }
$crt = Get-ChildItem (Join-Path $vs 'VC/Redist/MSVC') -Directory | Where-Object { $_.Name -match '^\d+\.' } |
    Sort-Object { [version]$_.Name } -Descending | ForEach-Object { Join-Path $_.FullName 'x64/Microsoft.VC143.CRT' } |
    Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
if (!$crt) { throw 'VS x64 redistributable CRT directory not found.' }
$pins = Get-Content -LiteralPath (Join-Path $root 'packaging/windows/media-pins.json') -Raw | ConvertFrom-Json
$license = Join-Path $root 'target/windows-vc-runtime-license.docx'
if (!(Test-Path -LiteralPath $license)) {
    New-Item (Split-Path $license) -ItemType Directory -Force | Out-Null
    Invoke-WebRequest $pins.vc_license_url -OutFile $license -TimeoutSec 120
}
if ((Get-FileHash -LiteralPath $license -Algorithm SHA256).Hash -ine $pins.vc_license_sha256) {
    throw 'Microsoft VC runtime license hash mismatch.'
}
$planFlag = if ($Plan) { @('--plan') } else { @() }
& $python $closure package --sdk $Sdk --crt $crt --crt-license $license @extra @planFlag $Output
if ($LASTEXITCODE -ne 0) { throw 'Media runtime packaging failed.' }
