# Installs, upgrades and uninstalls the per-user installer as the current user and checks what each step leaves:
# the files, the Start menu shortcut, the Add/Remove programs entry, the msteams: handler offered under Default apps,
# and that an uninstall keeps the user's data. Never starts ngteams (only -UpdateRelaunch does, see below).
#
# CI: scripts/test-windows-installer.ps1 -Installer dist/ngteams-<v>-windows-x86_64-setup.exe -Package <unzipped>
# Agents: the same in win11, on installers packaged from normal lab binaries, under a throwaway Windows user.
#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Installer,
    # An older installer: installed first, then upgraded in place by -Installer, then refused as a downgrade.
    [string]$Previous,
    # The unzipped release's ngteams folder: every installed program file must match its PACKAGE-MANIFEST.json hash.
    [string]$Package,
    # Upgrade the way the app's updater does (/UPDATEPID): waits for a stand-in process, then starts the installed
    # ngteams.exe. Before it, three updater runs that fail (a program still running from bin; a file held open in bin;
    # a late failure, the uninstaller held open) must leave the previous version whole and start it again. Only for lab-built installers (a lab build
    # refuses to start without --lab); never in CI.
    [switch]$UpdateRelaunch,
    [string]$Logs = (Join-Path ([IO.Path]::GetTempPath()) ('ngteams-installer-test-' + [Guid]::NewGuid().ToString('N')))
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$app = Join-Path $env:LOCALAPPDATA 'Programs\ngteams'
$exe = Join-Path $app 'bin\ngteams.exe'
$data = Join-Path $env:LOCALAPPDATA 'ngteams'
$uninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{1150C52E-F222-4AA3-B1D0-EB09F8BBBFF7}_is1'
$activator = 'Software\Classes\CLSID\{81B7D32D-75F7-6DB4-6A45-99728BC2AF17}'
$menu = Join-Path ([Environment]::GetFolderPath('Programs')) 'ngteams.lnk'
$choice = 'HKCU:\Software\Microsoft\Windows\Shell\Associations\UrlAssociations\msteams\UserChoice'
New-Item -ItemType Directory -Force $Logs | Out-Null

function Assert-That([bool]$Condition, [string]$What) {
    if (!$Condition) { throw "FAILED: $What" }
    Write-Output "ok: $What"
}

function Get-Version([string]$Path) {
    if ((Split-Path $Path -Leaf) -notmatch '^ngteams-(\d+\.\d+\.\d+\.\d+)-windows-x86_64-setup\.exe$') {
        throw "Not an ngteams installer name: $Path"
    }
    return $Matches[1]
}

function Get-RegistryValue([string]$Key, [string]$Name) {
    $item = Get-ItemProperty -LiteralPath $Key -ErrorAction SilentlyContinue
    if (!$item -or !($item.PSObject.Properties.Name -contains $Name)) { return $null }
    return $item.$Name
}

function Invoke-Setup([string]$Path, [string]$Name, [string[]]$Extra = @()) {
    $log = Join-Path $Logs "$Name.log"
    $arguments = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=`"$log`"") + $Extra
    $process = Start-Process -FilePath $Path -ArgumentList $arguments -PassThru -Wait
    Write-Output "$Name exited $($process.ExitCode) (log $log)"
    return $process.ExitCode
}

function Assert-Installed([string]$Version) {
    foreach ($file in 'bin\ngteams.exe', 'bin\ngteams-webview.exe', 'bin\ngteamsctl.exe', 'MEDIA-MANIFEST.json',
        'LICENSE-MIT', 'LICENSE-APACHE', 'share\licenses\ngteams\THIRD-PARTY.txt', 'unins000.exe') {
        Assert-That (Test-Path -LiteralPath (Join-Path $app $file) -PathType Leaf) "$file is installed"
    }
    foreach ($file in 'Install.ps1', 'Unregister.ps1', 'README.txt', 'PACKAGE-MANIFEST.json') {
        Assert-That (!(Test-Path -LiteralPath (Join-Path $app $file))) "the zip's $file isn't"
    }
    Assert-That ((Get-Content -Raw (Join-Path $app 'share\ngteams\update-format')).Trim() -eq 'windows-setup') 'the updater marker says windows-setup'
    Assert-That ((Get-Content -Raw (Join-Path $app 'share\ngteams\release')).Trim() -eq $Version) "the installed release is $Version"
    $shell = New-Object -ComObject WScript.Shell
    Assert-That ((Test-Path -LiteralPath $menu) -and $shell.CreateShortcut($menu).TargetPath -ieq $exe) 'the Start menu shortcut ngteams opens the installed app'
    $entry = "HKCU:\$uninstallKey"
    Assert-That ((Get-RegistryValue $entry 'DisplayName') -eq 'ngteams') 'Add/Remove programs lists ngteams'
    Assert-That ((Get-RegistryValue $entry 'DisplayVersion') -eq $Version) "Add/Remove programs shows version $Version"
    Assert-That ((Get-RegistryValue $entry 'Publisher') -eq 'ngteams') 'its publisher is ngteams'
    Assert-That (!(Test-Path -LiteralPath "HKLM:\$uninstallKey")) 'nothing is registered for all users'
    $named = @((Get-RegistryValue $entry 'DisplayName'), (Get-RegistryValue $entry 'Publisher'), (Split-Path $menu -Leaf), $app)
    Assert-That (!($named -match 'Microsoft Teams|MS Teams')) 'no name says Microsoft Teams'
    Assert-That ((Get-RegistryValue 'HKCU:\Software\RegisteredApplications' 'ngteams') -eq 'Software\ngteams\Capabilities') 'ngteams is a registered application'
    Assert-That ((Get-RegistryValue 'HKCU:\Software\ngteams\Capabilities\URLAssociations' 'msteams') -eq 'ngteams.msteams') 'it offers itself for msteams: links'
    Assert-That ((Get-RegistryValue 'HKCU:\Software\Classes\ngteams.msteams\shell\open\command' '(default)') -eq "`"$exe`" `"%1`"") 'the msteams: handler opens the installed app'
}

# The time Setup logged a line matching $Pattern at, from its log.
function Get-LogTime([string]$Log, [string]$Pattern) {
    $line = Select-String -LiteralPath $Log -Pattern $Pattern | Select-Object -First 1
    if (!$line -or $line.Line -notmatch '^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3})') { throw "FAILED: no '$Pattern' in $Log" }
    return [datetime]::ParseExact($Matches[1], 'yyyy-MM-dd HH:mm:ss.fff', [Globalization.CultureInfo]::InvariantCulture)
}

# Whether Setup's log shows it starting the installed ngteams.exe: a [Run] entry, or the fallback after a failure.
function Test-Restarted([string]$Log) {
    $text = Get-Content -Raw -LiteralPath $Log
    $run = "-- Run entry --(\r?\n.*){1,3}\r?\n.*Filename: $([regex]::Escape($exe))\r?\n"
    return ($text -match $run) -or ($text -match "Starting ngteams again: $([regex]::Escape($exe))")
}

# An updater run of -Installer over $Older that fails while $Hold is held open (read sharing only, as a program
# using it), or with no $Hold while a program runs from bin: it must exit non-zero, leave $Older whole with nothing set
# aside, and start it again.
function Assert-FailedUpdate([string]$Name, [string]$Hold, [string]$Older, [string]$Expected) {
    $standIn = Start-Process pwsh -ArgumentList '-NoProfile', '-Command', 'exit' -PassThru -Wait -WindowStyle Hidden
    if ($Hold) {
        $held = [IO.File]::Open($Hold, 'Open', 'Read', 'Read')
    } else {
        $runner = Join-Path $app 'bin\stand-in.exe'
        Copy-Item (Join-Path $env:SystemRoot 'System32\timeout.exe') $runner
        $held = Start-Process $runner -ArgumentList '/t', '300', '/nobreak' -PassThru -WindowStyle Hidden
    }
    try {
        $code = (Invoke-Setup $Installer $Name @("/UPDATEPID=$($standIn.Id)", '/SP-', "/DIR=`"$app`""))[-1]
    } finally {
        if ($Hold) {
            $held.Close()
        } else {
            Stop-Process -Id $held.Id -Force
            $held.WaitForExit()
            Remove-Item -LiteralPath $runner
        }
    }
    $log = Join-Path $Logs "$Name.log"
    Assert-That ($code -ne 0) "$Name fails"
    Assert-That ((Get-Content -Raw -LiteralPath $log) -match $Expected) "$Name failed where it was meant to ($Expected)"
    Assert-Installed $Older
    Assert-That (Test-Path -LiteralPath (Join-Path $app 'bin\previous-only.dll')) "$Name left the previous version's files"
    Assert-That (!(Test-Path -LiteralPath (Join-Path $app 'previous-version'))) "$Name left nothing set aside"
    Assert-That (Test-Restarted $log) "$Name started the previous ngteams.exe again"
}

function Assert-Package {
    if (!$Package) { return }
    $manifest = Get-Content -Raw (Join-Path $Package 'PACKAGE-MANIFEST.json') | ConvertFrom-Json
    $count = 0
    foreach ($file in $manifest.files.PSObject.Properties) {
        if ($file.Name -in 'Install.ps1', 'Unregister.ps1', 'README.txt') { continue }
        $installed = Join-Path $app $file.Name
        if (!(Test-Path -LiteralPath $installed) -or (Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash -ine $file.Value) {
            throw "FAILED: $($file.Name) differs from the release's manifest"
        }
        $count++
    }
    Write-Output "ok: all $count program files match the release's manifest"
}

$userChoice = Get-RegistryValue $choice 'ProgId'
$version = Get-Version $Installer
New-Item -ItemType Directory -Force (Join-Path $data 'data\updates\.ngteams-update-test') | Out-Null
Set-Content (Join-Path $data 'data\kept.txt') 'the user data the installer never touches'

if ($Previous) {
    $older = Get-Version $Previous
    Assert-That ((Invoke-Setup $Previous "install-$older")[-1] -eq 0) "ngteams $older installs"
    Assert-Installed $older
    # A file only the previous version had: the upgrade must not leave it.
    Set-Content (Join-Path $app 'bin\previous-only.dll') 'stale'
    if ($UpdateRelaunch) {
        # Stopped before anything moved: a program still runs from bin, or a file in it is in use.
        Assert-FailedUpdate "update-running-$version" '' $older 'ngteams is still running'
        Assert-FailedUpdate "update-held-$version" (Join-Path $app 'bin\previous-only.dll') $older 'is in use'
        # Failed after the previous version was moved aside and the new one copied: Setup can't replace the
        # uninstaller, rolls back, and the previous version goes back.
        Assert-FailedUpdate "update-late-$version" (Join-Path $app 'unins000.exe') $older 'Put the previous version''s bin back'
        # The updater's run: the installer waits for the app (a stand-in here) to exit, then starts the installed app.
        $standIn = Start-Process pwsh -ArgumentList '-NoProfile', '-Command', 'Start-Sleep 8' -PassThru -WindowStyle Hidden
        $code = (Invoke-Setup $Installer "update-$version" @("/UPDATEPID=$($standIn.Id)", '/SP-', "/DIR=`"$app`""))[-1]
        $log = Join-Path $Logs "update-$version.log"
        Assert-That ($code -eq 0) "the updater's run of $version succeeds"
        Assert-That ($standIn.HasExited -and (Get-LogTime $log 'Starting the installation process') -ge $standIn.ExitTime) 'it waited for the app to exit before installing'
        Assert-That (Test-Restarted $log) 'it started the installed ngteams.exe again'
    } else {
        Assert-That ((Invoke-Setup $Installer "upgrade-$version")[-1] -eq 0) "ngteams $older upgrades to $version in place"
    }
    Assert-That (!(Test-Path -LiteralPath (Join-Path $app 'bin\previous-only.dll'))) "the previous version's files are gone"
    Assert-That (!(Test-Path -LiteralPath (Join-Path $app 'previous-version'))) 'nothing is left set aside'
    Assert-Installed $version
    Assert-That (@(Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall' | Where-Object { (Get-ItemProperty $_.PSPath).PSObject.Properties.Name -contains 'DisplayName' -and (Get-ItemProperty $_.PSPath).DisplayName -eq 'ngteams' }).Count -eq 1) 'Add/Remove programs has one ngteams'
    Assert-That ((Invoke-Setup $Previous "downgrade-$older")[-1] -ne 0) "the older $older doesn't replace $version"
    Assert-Installed $version
    Assert-That ((Invoke-Setup $Previous "allowed-downgrade-$older" @('/ALLOWDOWNGRADE'))[-1] -eq 0) "/ALLOWDOWNGRADE puts $older back"
    Assert-Installed $older
    Assert-That ((Invoke-Setup $Installer "reupgrade-$version")[-1] -eq 0) "ngteams $older upgrades to $version again"
    Assert-Installed $version
} else {
    Assert-That ((Invoke-Setup $Installer "install-$version")[-1] -eq 0) "ngteams $version installs"
    Assert-Installed $version
}
Assert-Package
Assert-That ((Get-RegistryValue $choice 'ProgId') -eq $userChoice) "the user's msteams: default is unchanged"

# What the app registers on its first start, as it would (crates/teams-ui/src/desktop/windows/registration.rs).
New-Item -Force "HKCU:\$activator\LocalServer32" | Out-Null
Set-Item -LiteralPath "HKCU:\$activator\LocalServer32" -Value "`"$exe`""
New-Item -Force 'HKCU:\Software\Classes\AppUserModelId\com.nickalie.ngteams' | Out-Null
New-Item -ItemType Directory -Force (Join-Path $env:LOCALAPPDATA 'ngteams-desktop\com.nickalie.ngteams') | Out-Null

$uninstaller = Join-Path $app 'unins000.exe'
$process = Start-Process $uninstaller -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=`"$(Join-Path $Logs 'uninstall.log')`"" -PassThru -Wait
# The uninstaller runs on from a temporary copy; it is done when it has removed itself.
$deadline = (Get-Date).AddMinutes(3)
while ((Test-Path -LiteralPath $uninstaller) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 500 }
Start-Sleep 2
Assert-That ($process.ExitCode -eq 0 -and !(Test-Path -LiteralPath $uninstaller)) 'the uninstaller ran'
Assert-That (!(Test-Path -LiteralPath $app)) 'the program folder is gone'
Assert-That (!(Test-Path -LiteralPath $menu)) 'the Start menu shortcut is gone'
foreach ($key in "HKCU:\$uninstallKey", 'HKCU:\Software\Classes\ngteams.msteams', 'HKCU:\Software\ngteams',
    "HKCU:\$activator", 'HKCU:\Software\Classes\AppUserModelId\com.nickalie.ngteams') {
    Assert-That (!(Test-Path -LiteralPath $key)) "$key is gone"
}
Assert-That ($null -eq (Get-RegistryValue 'HKCU:\Software\RegisteredApplications' 'ngteams')) 'ngteams is no registered application any more'
Assert-That (!(Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA 'ngteams-desktop\com.nickalie.ngteams'))) "the app's notification icon folder is gone"
Assert-That (!(Test-Path -LiteralPath (Join-Path $data 'data\updates'))) 'update downloads are gone'
Assert-That ((Get-Content (Join-Path $data 'data\kept.txt')) -eq 'the user data the installer never touches') "the user's data is kept"
Assert-That ((Get-RegistryValue $choice 'ProgId') -eq $userChoice) "the user's msteams: default is still unchanged"
Write-Output "PASSED (logs in $Logs)"
