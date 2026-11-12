# Package prebuilt binaries. The release workflow builds the install-marked app; agents use lab binaries only.
# With -InnoSetup, the per-user installer (packaging/windows/ngteams.iss) is built from the same staged, checked files
# as the zip and put beside it.
#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Binaries,
    [Parameter(Mandatory)][ValidatePattern('^\d+\.\d+\.\d+\.\d+$')][string]$Version,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-f]{40}$')][string]$Commit,
    [string]$Output = 'dist',
    [string]$Runtime,
    # ISCC.exe of the pinned Inno Setup (scripts/release-innosetup.ps1).
    [string]$InnoSetup,
    # The code-signing hook, unused until there is a certificate (docs/ci.md#windows-code-signing): an Inno Setup sign
    # tool command line, `$f` for the quoted file and `$q` for a quote, e.g.
    # 'signtool.exe sign /fd sha256 /td sha256 /tr <timestamp URL> /f <certificate> $f'. It signs the app's three
    # programs before they are packaged, and the installer and its uninstaller.
    [string]$SignCommand
)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
$Binaries = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Binaries)
$Output = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Output)
if ($SignCommand -and ($SignCommand -notmatch '\$f' -or $SignCommand.Contains('"'))) {
    throw 'The sign command needs $f for the file, and $q rather than a double quote.'
}
# The top-level names ngteams.iss's [InstallDelete] knows: an upgrade removes exactly these before installing.
$installerNames = @('bin', 'lib', 'libexec', 'share', 'MEDIA-MANIFEST.json', 'LICENSE-MIT', 'LICENSE-APACHE',
    'Install.ps1', 'Unregister.ps1', 'README.txt', 'PACKAGE-MANIFEST.json')

function Invoke-NgteamsSigning([string]$File) {
    $line = $SignCommand.Replace('$f', '"' + $File + '"').Replace('$q', '"')
    $process = Start-Process cmd.exe -ArgumentList ('/d /s /c "' + $line + '"') -NoNewWindow -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "Signing $File failed ($($process.ExitCode))." }
}

function New-NgteamsInstaller([string]$Stage) {
    $unknown = @(Get-ChildItem -LiteralPath $Stage -Force | Where-Object { $installerNames -notcontains $_.Name })
    if ($unknown) { throw "Add these to ngteams.iss [InstallDelete] and this script: $($unknown.Name -join ', ')" }
    # What the installed app's updater reads (crates/teams-update MARKER and IDENTITY), installed under share/ngteams.
    $markers = Join-Path ([IO.Path]::GetTempPath()) ('ngteams-installer-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory $markers | Out-Null
    try {
        [IO.File]::WriteAllText((Join-Path $markers 'update-format'), "windows-setup`n")
        [IO.File]::WriteAllText((Join-Path $markers 'release'), "$Version`n")
        $arguments = @('/Q', "/DAppVersion=$Version", "/DStageDir=$Stage", "/DMarkerDir=$markers", "/DOutputDir=$Output")
        if ($SignCommand) { $arguments += @('/DSignTool=ngteams', "/Sngteams=$SignCommand") }
        & $InnoSetup @arguments (Join-Path $root 'packaging/windows/ngteams.iss')
        if ($LASTEXITCODE -ne 0) { throw "The Windows installer couldn't be built ($LASTEXITCODE)." }
    } finally { Remove-Item -LiteralPath $markers -Recurse -Force }
    $setup = Join-Path $Output "ngteams-$Version-windows-x86_64-setup.exe"
    if (!(Test-Path -LiteralPath $setup)) { throw 'The Windows installer is missing.' }
    return $setup
}
# rust-cache prunes non-Cargo directories under target before saving them. Keep generated media outside that
# tree, and use a fresh path so updated SDK/plugin pins can never reuse a stale local runtime either.
$ownsRuntime = !$Runtime
if ($ownsRuntime) { $Runtime = Join-Path ([IO.Path]::GetTempPath()) ('ngteams-release-media-' + [Guid]::NewGuid().ToString('N')) }
try {
    $programs = @('ngteams.exe', 'ngteams-webview.exe', 'ngteamsctl.exe') | ForEach-Object { Join-Path $Binaries $_ }
    if ($SignCommand) { foreach ($program in $programs) { Invoke-NgteamsSigning $program } }
    & "$PSScriptRoot/package-windows-media.ps1" -Output $Runtime -Binary $programs
    $notices = Join-Path $root ('target/windows-release-notices-' + [Guid]::NewGuid().ToString('N'))
    & python "$PSScriptRoot/windows-release-notices.py" --target $Binaries $notices
    if ($LASTEXITCODE -ne 0) { throw 'Windows dependency notices could not be collected.' }
    $stage = Join-Path $Output ('stage-' + $Version)
    $archive = Join-Path $Output "ngteams-$Version-windows-x86_64.zip"
    & python "$PSScriptRoot/windows-release.py" stage --binaries $Binaries --runtime $Runtime `
        --version $Version --commit $Commit --output $stage --archive $archive --notices $notices
    if ($LASTEXITCODE -ne 0) { throw 'Windows release packaging failed.' }
    if ($InnoSetup) { $setup = New-NgteamsInstaller $stage }
    # Only the zip and the installer are public assets: the stage goes once both are made.
    Remove-Item -LiteralPath $stage, $notices -Recurse -Force
    Write-Output "Windows archive: $archive"
    if ($InnoSetup) { Write-Output "Windows installer: $setup" }
} finally {
    if ($ownsRuntime -and (Test-Path -LiteralPath $Runtime)) { Remove-Item -LiteralPath $Runtime -Recurse -Force }
}
