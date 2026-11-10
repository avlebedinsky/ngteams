# UI Automation stays inside Explorer's taskbar/overflow and this synthetic process's native popup.
function Initialize-NgteamsShellAutomation {
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
    Add-Type -AssemblyName System.Drawing
    if (!('NgteamsShellInput' -as [type])) {
        Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class NgteamsShellInput {
    [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr window);
    [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr window, IntPtr dc);
    [DllImport("gdi32.dll")] public static extern bool BitBlt(IntPtr target, int x, int y, int width, int height, IntPtr source, int sx, int sy, uint operation);
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr window, int command);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr window, uint message, UIntPtr wparam, IntPtr lparam);
}
'@
    }
    # UIA rectangles are physical pixels. Keep capture and pointer coordinates in that same space.
    if ([NgteamsShellInput]::SetThreadDpiAwarenessContext([IntPtr]::new(-4)) -eq [IntPtr]::Zero) {
        throw 'Cannot select physical-pixel DPI coordinates for the console driver.'
    }
}

function Copy-NgteamsShellPixels($Graphics, $Size, [int]$X, [int]$Y) {
    $target = $Graphics.GetHdc()
    $screen = [NgteamsShellInput]::GetDC([IntPtr]::Zero)
    try {
        # CAPTUREBLT includes Explorer's layered windows; GDI+ rejects that flag combined with SourceCopy.
        if (!$screen -or ![NgteamsShellInput]::BitBlt($target,0,0,$Size.Width,$Size.Height,$screen,$X,$Y,0x40CC0020)) {
            throw 'Cannot capture the verified shell rectangle.'
        }
    } finally {
        if ($screen) { $null = [NgteamsShellInput]::ReleaseDC([IntPtr]::Zero,$screen) }
        $Graphics.ReleaseHdc($target)
    }
}

function Get-NgteamsTaskbar {
    $condition = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ClassNameProperty, 'Shell_TrayWnd')
    return [Windows.Automation.AutomationElement]::RootElement.FindFirst([Windows.Automation.TreeScope]::Children, $condition)
}

function Save-NgteamsShellElement($Element, [string]$Path) {
    if ($Element.Current.IsOffscreen) { throw 'Cannot capture an offscreen shell element.' }
    $rect = $Element.Current.BoundingRectangle
    if ($rect.Width -le 0 -or $rect.Height -le 0 -or $rect.Width -gt 4096 -or $rect.Height -gt 256) {
        throw 'Invalid shell evidence rectangle.'
    }
    @{left=$rect.Left;top=$rect.Top;width=$rect.Width;height=$rect.Height} | ConvertTo-Json -Compress | Set-Content ($Path+'.rectangle.json')
    $bitmap = [Drawing.Bitmap]::new([int]$rect.Width, [int]$rect.Height,[Drawing.Imaging.PixelFormat]::Format24bppRgb)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    try {
        Copy-NgteamsShellPixels $graphics $bitmap.Size ([int]$rect.Left) ([int]$rect.Top)
        if ($Element.Current.IsOffscreen -or $Element.Current.BoundingRectangle -ne $rect) {
            throw 'Shell element moved during capture.'
        }
        $bitmap.Save($Path,[Drawing.Imaging.ImageFormat]::Png)
    } finally { $graphics.Dispose(); $bitmap.Dispose() }
}

function Save-NgteamsShellRectangle([string]$Directory, [string]$Name) {
    $rect = Get-Content (Join-Path $Directory ($Name+'.rectangle.json')) -Raw | ConvertFrom-Json
    $width = [int]($rect.right-$rect.left); $height = [int]($rect.bottom-$rect.top)
    if ($width -le 0 -or $height -le 0 -or $width -gt 700 -or $height -gt 256) { throw 'Invalid shell capture rectangle.' }
    $bitmap = [Drawing.Bitmap]::new($width,$height,[Drawing.Imaging.PixelFormat]::Format24bppRgb)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    try {
        Copy-NgteamsShellPixels $graphics $bitmap.Size ([int]$rect.left) ([int]$rect.top)
        $bitmap.Save((Join-Path $Directory ($Name+'.png')),[Drawing.Imaging.ImageFormat]::Png)
    } finally { $graphics.Dispose(); $bitmap.Dispose() }
    return $rect
}

function Invoke-NgteamsShellRectangle($Rect, [switch]$Right) {
    if (![NgteamsShellInput]::SetCursorPos([int](($Rect.left+$Rect.right)/2), [int](($Rect.top+$Rect.bottom)/2))) {
        throw 'Cannot move pointer to the verified synthetic shell control.'
    }
    $down = if ($Right) { 0x0008 } else { 0x0002 }
    $up = if ($Right) { 0x0010 } else { 0x0004 }
    [NgteamsShellInput]::mouse_event($down,0,0,0,[UIntPtr]::Zero)
    Start-Sleep -Milliseconds 50
    [NgteamsShellInput]::mouse_event($up,0,0,0,[UIntPtr]::Zero)
}

function Open-NgteamsShellMenu([Diagnostics.Process]$Process, [string]$Directory) {
    $driver = Start-Process $Process.Path -ArgumentList '--drive-tray' -NoNewWindow -PassThru `
        -RedirectStandardOutput (Join-Path $Directory 'driver.out') -RedirectStandardError (Join-Path $Directory 'driver.err')
    @{ id=$driver.Id; started=$driver.StartTime.ToUniversalTime().Ticks } | ConvertTo-Json -Compress |
        Set-Content -LiteralPath (Join-Path $Directory 'driver-process.json')
    try {
        foreach ($step in @('tray','menu','open-item')) {
            $deadline = [DateTime]::UtcNow.AddSeconds(12)
            while (!(Test-Path (Join-Path $Directory ($step+'.rectangle.json')))) {
                if ($step -eq 'tray' -and (Test-Path (Join-Path $Directory 'expand.rectangle.json')) -and
                    !(Test-Path (Join-Path $Directory 'expand.captured'))) {
                    $expand = Save-NgteamsShellRectangle $Directory 'expand'
                    Invoke-NgteamsShellRectangle $expand
                    Set-Content (Join-Path $Directory 'expand.captured') 'captured and clicked'
                }
                $driver.Refresh()
                if ($driver.HasExited -or [DateTime]::UtcNow -ge $deadline) { throw "Native tray driver failed before $step." }
                Start-Sleep -Milliseconds 50
            }
            if ($step -eq 'open-item') {
                $rect = Get-Content (Join-Path $Directory ($step+'.rectangle.json')) -Raw | ConvertFrom-Json
                Invoke-NgteamsShellRectangle $rect
            } else {
                $rect = Save-NgteamsShellRectangle $Directory $step
                if ($step -eq 'tray') { Invoke-NgteamsShellRectangle $rect -Right }
                Set-Content (Join-Path $Directory ($step+'.captured')) 'captured'
            }
        }
        if (!$driver.WaitForExit(1000) -or $driver.ExitCode -ne 0) { throw 'Native tray driver did not pass.' }
    } finally {
        $driver.Refresh()
        if (!$driver.HasExited) { $driver.Kill($true); $driver.WaitForExit() }
    }
}

function Invoke-NgteamsSyntheticShell([Diagnostics.Process]$Process, [string]$Directory) {
    [IO.File]::AppendAllText((Join-Path $Directory 'driver.log'), 'initializing'+[Environment]::NewLine)
    Initialize-NgteamsShellAutomation
    [IO.File]::AppendAllText((Join-Path $Directory 'driver.log'), 'initialized'+[Environment]::NewLine)
    foreach ($step in @('plain','minimize','active','unread','settings','dark','close','open','reopened','chat','read')) {
        $deadline = [DateTime]::UtcNow.AddSeconds(18)
        do {
            $Process.Refresh()
            if ($Process.HasExited) { throw "Synthetic shell exited before $step." }
            $ready = (Test-Path (Join-Path $Directory 'step')) -and
                (Get-Content (Join-Path $Directory 'step') -Raw).Trim() -eq $step
            if (!$ready) { Start-Sleep -Milliseconds 50 }
        } while (!$ready -and [DateTime]::UtcNow -lt $deadline)
        if (!$ready) { throw "Synthetic shell checkpoint deadline: $step" }
        [IO.File]::AppendAllText((Join-Path $Directory 'driver.log'), $step+[Environment]::NewLine)
        if ($step -in @('close','minimize')) {
            if ($Process.MainWindowTitle -ne 'NGTEAMS SYNTHETIC SHELL' -or !$Process.MainWindowHandle) {
                throw 'Close target is not the synthetic GPUI window.'
            }
            if ($step -eq 'minimize') {
                $null = [NgteamsShellInput]::ShowWindow($Process.MainWindowHandle,6)
            } elseif (![NgteamsShellInput]::PostMessage($Process.MainWindowHandle,0x10,[UIntPtr]::Zero,[IntPtr]::Zero)) {
                throw 'Cannot close synthetic GPUI window.'
            }
            Start-Sleep -Milliseconds 400
        } elseif ($step -eq 'open') {
            Open-NgteamsShellMenu $Process $Directory
            Start-Sleep -Milliseconds 400
        } else {
            [IO.File]::AppendAllText((Join-Path $Directory 'driver.log'), 'client shot'+[Environment]::NewLine)
            if (!(Save-NgteamsSyntheticShot $Process 'NGTEAMS SYNTHETIC SHELL' (Join-Path $Directory ($step+'.png')))) {
                throw "Cannot capture foreground synthetic GPUI window: $step"
            }
            if ($step -in @('plain','active','unread','read','reopened')) {
                [IO.File]::AppendAllText((Join-Path $Directory 'driver.log'), 'taskbar shot'+[Environment]::NewLine)
                $taskbar = Get-NgteamsTaskbar
                $condition = [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::AutomationIdProperty,
                    ('Appid: com.nickalie.ngteams.synthetic.'+$Process.Id))
                $button = $taskbar.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
                if (!$button) { throw 'Synthetic app taskbar button is missing.' }
                Save-NgteamsShellElement $button (Join-Path $Directory ($step+'-taskbar.png'))
            }
        }
        Set-Content (Join-Path $Directory ($step+'.done')) 'done'
    }
}
