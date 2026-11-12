# Remove only this package's active shortcuts and link registration. Account files are never touched.
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$exe = Join-Path $PSScriptRoot 'bin/ngteams.exe'
$command = 'HKCU:/Software/Classes/ngteams.msteams/shell/open/command'
$expected = '"' + $exe + '" "%1"'
if ((Test-Path -LiteralPath $command) -and (Get-Item -LiteralPath $command).GetValue('') -eq $expected) {
    Remove-Item -LiteralPath 'HKCU:/Software/Classes/ngteams.msteams' -Recurse
    Remove-Item -LiteralPath 'HKCU:/Software/ngteams/Capabilities' -Recurse -ErrorAction SilentlyContinue
    Remove-ItemProperty -LiteralPath 'HKCU:/Software/RegisteredApplications' -Name ngteams -ErrorAction SilentlyContinue
}
$menu = Join-Path ([Environment]::GetFolderPath('Programs')) 'ngteams.lnk'
if (Test-Path -LiteralPath $menu) {
    $shell = New-Object -ComObject WScript.Shell
    if ($shell.CreateShortcut($menu).TargetPath -ieq $exe) { Remove-Item -LiteralPath $menu }
}
# Remove the installed app's toast metadata only if its registered server still points at this version.
$hash = [Security.Cryptography.SHA256]::Create()
try { $bytes = $hash.ComputeHash([Text.Encoding]::UTF8.GetBytes('ngteams.toast.activator:com.nickalie.ngteams')) }
finally { $hash.Dispose() }
$hex = -join ($bytes[0..15] | ForEach-Object { $_.ToString('x2') })
$clsid = '{' + ([guid]::ParseExact($hex,'N')).ToString() + '}'
$server = "HKCU:/Software/Classes/CLSID/$clsid/LocalServer32"
if ((Test-Path -LiteralPath $server) -and (Get-Item -LiteralPath $server).GetValue('') -eq ('"' + $exe + '"')) {
    Remove-Item -LiteralPath (Split-Path $server) -Recurse
    Remove-Item -LiteralPath 'HKCU:/Software/Classes/AppUserModelId/com.nickalie.ngteams' -Recurse -ErrorAction SilentlyContinue
}
Write-Output 'Removed registrations owned by this version. Close ngteams before deleting its program folder. Account data is retained.'
