# Per-user setup. Never starts ngteams. Windows PowerShell 5.1 is sufficient.
# Run -VerifyOnly to validate the extracted package without installing anything.
[CmdletBinding()]
param([switch]$VerifyOnly)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Test-NgteamsPackage([string]$Directory) {
    $manifest = Get-Content -LiteralPath (Join-Path $Directory 'PACKAGE-MANIFEST.json') -Raw | ConvertFrom-Json
    if ($manifest.version -notmatch '^\d+\.\d+\.\d+\.\d+$' -or $manifest.commit -notmatch '^[0-9a-f]{40}$' -or
        $manifest.arch -ne 'x86_64' -or $manifest.platform -ne 'windows') { throw 'Invalid Windows package metadata.' }
    $expected = @{}
    foreach ($file in $manifest.files.PSObject.Properties) {
        if ($file.Name -match '(^/|\\|:|(^|/)\.\.(/|$))' -or $file.Value -notmatch '^[0-9a-f]{64}$') {
            throw 'Invalid package manifest path or hash.'
        }
        if ($expected.ContainsKey($file.Name)) { throw 'Duplicate package path.' }
        $expected[$file.Name] = $file.Value
    }
    $count = 0
    foreach ($file in Get-ChildItem -LiteralPath $Directory -Recurse -Force) {
        if ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Package contains a reparse point.' }
        if ($file.PSIsContainer) { continue }
        $relative = $file.FullName.Substring($Directory.TrimEnd('\','/').Length + 1).Replace('\','/')
        if ($relative -eq 'PACKAGE-MANIFEST.json') { continue }
        if (!$expected.ContainsKey($relative) -or (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash -ine $expected[$relative]) {
            throw "Package file missing, changed or unexpected: $relative"
        }
        $count++
    }
    if ($count -ne $expected.Count) { throw 'Package has missing files.' }
    foreach ($required in @('bin/ngteams.exe','bin/ngteams-webview.exe','bin/ngteamsctl.exe','MEDIA-MANIFEST.json',
        'Install.ps1','Unregister.ps1','README.txt','share/licenses/ngteams/THIRD-PARTY.txt')) {
        if (!$expected.ContainsKey($required)) { throw "Package is incomplete: $required" }
    }
    return $manifest
}

function Test-WebView2Runtime {
    $client = 'Microsoft/EdgeUpdate/Clients/{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
    foreach ($key in @("HKLM:/SOFTWARE/WOW6432Node/$client", "HKCU:/SOFTWARE/$client")) {
        $value = Get-ItemProperty -LiteralPath $key -Name pv -ErrorAction SilentlyContinue
        if ($value -and $value.pv) {
            $parsed = $null
            if ([version]::TryParse($value.pv, [ref]$parsed) -and $parsed -gt [version]'0.0.0.0') { return $true }
        }
    }
    return $false
}

$source = [IO.Path]::GetFullPath($PSScriptRoot)
$package = Test-NgteamsPackage $source
if ($VerifyOnly) { Write-Output "Verified ngteams $($package.version), commit $($package.commit)"; return }
if (![Environment]::Is64BitProcess -or [Environment]::OSVersion.Version.Build -lt 22000) { throw 'Windows 11 x64 is required. Use 64-bit PowerShell.' }
if (([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run setup as your ordinary user, without elevation.' }
if (!(Test-WebView2Runtime)) {
    $bootstrap = Join-Path ([IO.Path]::GetTempPath()) ('ngteams-webview2-' + [Guid]::NewGuid().ToString('N') + '.exe')
    try {
        # Microsoft's documented Evergreen bootstrapper; signature verified before it is executed.
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest 'https://go.microsoft.com/fwlink/p/?LinkId=2124703' -UseBasicParsing -OutFile $bootstrap -TimeoutSec 120
        $signature = Get-AuthenticodeSignature -LiteralPath $bootstrap
        if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch '(^|,\s*)O=Microsoft Corporation(,|$)') {
            throw 'WebView2 installer is not signed by Microsoft.'
        }
        $process = Start-Process $bootstrap -ArgumentList '/silent','/install' -PassThru
        if (!$process.WaitForExit(600000)) { $process.Kill(); throw 'WebView2 installation deadline expired.' }
        if ($process.ExitCode -ne 0 -or !(Test-WebView2Runtime)) { throw 'WebView2 installation failed; install or repair the Microsoft WebView2 Runtime and retry.' }
    } finally { Remove-Item -LiteralPath $bootstrap -Force -ErrorAction SilentlyContinue }
}
$destination = Join-Path $env:LOCALAPPDATA "Programs/ngteams/$($package.version)"
if (Test-Path -LiteralPath $destination) {
    $existing = Test-NgteamsPackage $destination
    if ($existing.commit -ne $package.commit) { throw 'This version already exists with a different commit.' }
} else {
    $parent = Split-Path $destination
    New-Item -ItemType Directory -Path $parent -Force | Out-Null
    $temporary = Join-Path $parent ('.setup-' + [Guid]::NewGuid().ToString('N'))
    try {
        Copy-Item -LiteralPath $source -Destination $temporary -Recurse
        $null = Test-NgteamsPackage $temporary
        Move-Item -LiteralPath $temporary -Destination $destination
    } finally { if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Recurse -Force } }
}
$exe = Join-Path $destination 'bin/ngteams.exe'
$menu = Join-Path ([Environment]::GetFolderPath('Programs')) 'ngteams.lnk'
$shell = New-Object -ComObject WScript.Shell
$link = $shell.CreateShortcut($menu)
$link.TargetPath = $exe
$link.WorkingDirectory = Split-Path $exe
$link.IconLocation = "$exe,0"
$link.Description = 'ngteams'
$link.Save()
# On its first normal launch, the app sets this shortcut's AppUserModelID and toast activator properties and
# registers its native COM notification server. Setup does not launch a sending client to do that.
$program = 'HKCU:/Software/Classes/ngteams.msteams'
New-Item -Path "$program/shell/open/command" -Force | Out-Null
Set-Item -LiteralPath $program -Value 'ngteams Teams meeting link'
New-ItemProperty -LiteralPath $program -Name 'URL Protocol' -Value '' -Force | Out-Null
Set-Item -LiteralPath "$program/shell/open/command" -Value ('"' + $exe + '" "%1"')
$capabilities = 'HKCU:/Software/ngteams/Capabilities'
New-Item -Path "$capabilities/URLAssociations" -Force | Out-Null
New-ItemProperty -LiteralPath $capabilities -Name ApplicationName -Value ngteams -Force | Out-Null
New-ItemProperty -LiteralPath $capabilities -Name ApplicationDescription -Value 'Native Microsoft Teams client' -Force | Out-Null
New-ItemProperty -LiteralPath "$capabilities/URLAssociations" -Name msteams -Value ngteams.msteams -Force | Out-Null
New-Item -Path 'HKCU:/Software/RegisteredApplications' -Force | Out-Null
New-ItemProperty -LiteralPath 'HKCU:/Software/RegisteredApplications' -Name ngteams -Value 'Software\ngteams\Capabilities' -Force | Out-Null
Write-Output "Installed ngteams $($package.version). Open ngteams from Start to sign in."
Write-Output 'To open Teams links here, select ngteams for MSTeams in Windows Settings > Apps > Default apps.'
