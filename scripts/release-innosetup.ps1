# Install only the hash-pinned Inno Setup compiler that builds the Windows installer (packaging/windows/ngteams.iss),
# for this user, into a folder of its own. Prints the ISCC.exe path.
#Requires -Version 7.0
[CmdletBinding()]
param([string]$Destination = (Join-Path (Split-Path $PSScriptRoot) 'windows-deps/innosetup'))
$ErrorActionPreference = 'Stop'
$pins = Get-Content "$PSScriptRoot/../packaging/windows/installer-pins.json" -Raw | ConvertFrom-Json
$Destination = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Destination)
if (Test-Path -LiteralPath $Destination) { throw 'Inno Setup destination exists; use a clean destination.' }
$installer = Join-Path ([IO.Path]::GetTempPath()) ('ngteams-innosetup-' + [Guid]::NewGuid().ToString('N') + '.exe')
try {
    Invoke-WebRequest $pins.innosetup_url -OutFile $installer -TimeoutSec 600
    if ((Get-FileHash $installer -Algorithm SHA256).Hash -ine $pins.innosetup_sha256) { throw 'Inno Setup installer hash mismatch.' }
    $arguments = @('/CURRENTUSER', '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/NOICONS', '/NOCANCEL', ('/DIR="' + $Destination + '"'))
    $process = Start-Process $installer -ArgumentList $arguments -PassThru
    if (!$process.WaitForExit(600000)) { $process.Kill($true); throw 'Inno Setup installation deadline expired.' }
    if ($process.ExitCode -ne 0) { throw "Inno Setup installer failed ($($process.ExitCode))." }
    $iscc = Join-Path $Destination 'ISCC.exe'
    if (!(Test-Path -LiteralPath $iscc)) { throw 'ISCC.exe is missing after the Inno Setup installation.' }
    $version = & $iscc --version
    if ($LASTEXITCODE -ne 0 -or "$version" -notmatch [regex]::Escape($pins.innosetup_version)) { throw "Unexpected Inno Setup compiler: $version" }
    Write-Output $iscc
} finally { Remove-Item -LiteralPath $installer -Force -ErrorAction SilentlyContinue }
