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
    [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] static extern IntPtr GetThreadDesktop(uint thread);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern bool GetUserObjectInformation(
        IntPtr desktop, int index, out int value, uint length, out uint needed);
    public static bool IsInputDesktop() {
        int input; uint needed;
        // UOI_IO: a secure desktop can cover the frame while this desktop still reports its own foreground HWND.
        return GetUserObjectInformation(GetThreadDesktop(GetCurrentThreadId()), 6, out input, 4, out needed) && input != 0;
    }
}
'@
    }
    $handle=$Process.MainWindowHandle
    if (![NgteamsSyntheticShot]::IsInputDesktop() -or [NgteamsSyntheticShot]::GetForegroundWindow() -ne $handle) { return $false }
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
        if (![NgteamsSyntheticShot]::IsInputDesktop() -or [NgteamsSyntheticShot]::GetForegroundWindow() -ne $handle) { return $false }
        $bitmap.Save($OutputFile,[Drawing.Imaging.ImageFormat]::Png)
    } finally { $graphics.Dispose(); $bitmap.Dispose() }
    return $true
}
