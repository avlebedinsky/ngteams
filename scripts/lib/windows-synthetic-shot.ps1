# Client-area screenshots of the current synthetic fixture only, from its authorized VM console task.
function Save-NgteamsSyntheticShot([Diagnostics.Process]$Process, [string]$Title, [string]$OutputFile) {
    $Process.Refresh()
    if ($Process.HasExited -or $Process.MainWindowTitle -ne $Title -or !$Process.MainWindowHandle) { return $false }
    Add-Type -AssemblyName System.Drawing
    if (!('NgteamsSyntheticShot' -as [type])) {
        Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class NgteamsSyntheticShot {
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr window, ref Point point);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
}
'@
    }
    $handle=$Process.MainWindowHandle
    if ([NgteamsSyntheticShot]::GetForegroundWindow() -ne $handle) { return $false }
    $rect=[NgteamsSyntheticShot+Rect]::new()
    $point=[NgteamsSyntheticShot+Point]::new()
    if (![NgteamsSyntheticShot]::GetClientRect($handle,[ref]$rect) -or
        ![NgteamsSyntheticShot]::ClientToScreen($handle,[ref]$point)) { return $false }
    if ($rect.Right -le 0 -or $rect.Bottom -le 0) { return $false }
    $bitmap=[Drawing.Bitmap]::new($rect.Right,$rect.Bottom)
    $graphics=[Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen($point.X,$point.Y,0,0,$bitmap.Size)
        # Recheck focus before retaining pixels; no full-desktop file is ever created.
        if ([NgteamsSyntheticShot]::GetForegroundWindow() -ne $handle) { return $false }
        $bitmap.Save($OutputFile,[Drawing.Imaging.ImageFormat]::Png)
    } finally { $graphics.Dispose(); $bitmap.Dispose() }
    return $true
}
