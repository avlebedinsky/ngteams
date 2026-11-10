# Capture only a native-UIA-verified synthetic toast rectangle, then let that client invoke its retained element.
function Save-NgteamsSyntheticToast([string]$Directory, [string]$Action) {
    $path=Join-Path $Directory ($Action+'.rectangle.json')
    if (!(Test-Path -LiteralPath $path)) { return $false }
    $rect=Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
    $width=[int]($rect.right-$rect.left); $height=[int]($rect.bottom-$rect.top)
    if ($width -lt 200 -or $width -gt 700 -or $height -lt 70 -or $height -gt 450) { throw 'Invalid synthetic toast rectangle.' }
    Add-Type -AssemblyName System.Drawing
    $bitmap=[Drawing.Bitmap]::new($width,$height)
    $graphics=[Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen([int]$rect.left,[int]$rect.top,0,0,$bitmap.Size)
        $bitmap.Save((Join-Path $Directory ($Action+'.png')),[Drawing.Imaging.ImageFormat]::Png)
    } finally { $graphics.Dispose(); $bitmap.Dispose() }
    Set-Content -LiteralPath (Join-Path $Directory ($Action+'.captured')) -Value 'captured synthetic toast'
    return $true
}

function Remove-NgteamsSyntheticShellIdentity([int]$OwnedProcessId) {
    $identity='com.nickalie.ngteams.synthetic.'+$OwnedProcessId
    # Derive exactly this fixture's class; never delete a CLSID taken from mutable shell metadata.
    $digest=[Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes('ngteams.toast.activator:'+$identity))
    $hex=[Convert]::ToHexString($digest).Substring(0,32)
    $activator=[guid]::ParseExact($hex,'N').ToString('B')
    Remove-Item -LiteralPath ('HKCU:\Software\Classes\CLSID\'+$activator) -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath ('HKCU:\Software\Microsoft\Windows\CurrentVersion\Notifications\Settings\'+$identity) -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath ('HKCU:\Software\Classes\AppUserModelId\'+$identity) -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath (Join-Path $env:LOCALAPPDATA ('ngteams-desktop/'+$identity)) -Recurse -Force -ErrorAction SilentlyContinue
    $shortcut=Join-Path $env:APPDATA ('Microsoft/Windows/Start Menu/Programs/'+$identity+'.'+$OwnedProcessId+'.lnk')
    Remove-Item -LiteralPath $shortcut -Force -ErrorAction SilentlyContinue
}
