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
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [StructLayout(LayoutKind.Sequential)] struct MonitorInfo { public int Size; public Rect Monitor, Work; public uint Flags; }
    [DllImport("user32.dll")] static extern IntPtr MonitorFromWindow(IntPtr window, uint flags);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern bool GetMonitorInfo(IntPtr monitor, ref MonitorInfo info);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] static extern IntPtr WindowFromPoint(Point point);
    [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr window, uint flags);
    delegate bool WindowVisitor(IntPtr window, IntPtr parameter);
    [DllImport("user32.dll")] static extern bool EnumWindows(WindowVisitor visitor, IntPtr parameter);
    public static bool CanCapture(IntPtr window, Point origin, Rect client) {
        if (!IsInputDesktop() || GetForegroundWindow() != window) return false;
        var info = new MonitorInfo { Size = Marshal.SizeOf(typeof(MonitorInfo)) };
        if (!GetMonitorInfo(MonitorFromWindow(window, 2), ref info)) return false;
        int right = origin.X + client.Right, bottom = origin.Y + client.Bottom;
        // CopyFromScreen reads the desktop: never retain a clipped window or the taskbar covering its client.
        if (origin.X < info.Work.Left || origin.Y < info.Work.Top || right > info.Work.Right || bottom > info.Work.Bottom) return false;
        bool found = false, clear = true;
        EnumWindows((other, unused) => {
            if (other == window) { found = true; return false; }
            Rect rect;
            if (!IsWindowVisible(other) || !GetWindowRect(other, out rect)) return true;
            int left = Math.Max(origin.X, rect.Left), top = Math.Max(origin.Y, rect.Top);
            int edge = Math.Min(right, rect.Right), foot = Math.Min(bottom, rect.Bottom);
            if (left >= edge || top >= foot) return true;
            var hit = WindowFromPoint(new Point { X = left + (edge-left)/2, Y = top + (foot-top)/2 });
            // Ignore windows that do not cover this point (e.g. cloaked or input-transparent shell overlays).
            if (GetAncestor(hit, 2) != window) { clear = false; return false; }
            return true;
        }, IntPtr.Zero);
        return found && clear;
    }
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
    # ClientToScreen and GDI must agree on physical pixels, including a fixture on a scaled display.
    $previousDpi=[NgteamsSyntheticShot]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
    if ($previousDpi -eq [IntPtr]::Zero) { return $false }
    try {
        if (![NgteamsSyntheticShot]::IsInputDesktop()) { return $false }
        $rect=[NgteamsSyntheticShot+Rect]::new()
        $point=[NgteamsSyntheticShot+Point]::new()
        if (![NgteamsSyntheticShot]::GetClientRect($handle,[ref]$rect) -or
            ![NgteamsSyntheticShot]::ClientToScreen($handle,[ref]$point)) { return $false }
        if ($rect.Right -le 0 -or $rect.Bottom -le 0) { return $false }
        if (![NgteamsSyntheticShot]::CanCapture($handle,$point,$rect)) {
            # Data-free diagnostics for the owned fixture; never retain the rejected desktop pixels.
            @{ foreground=([NgteamsSyntheticShot]::GetForegroundWindow() -eq $handle); origin=$point; client=$rect } |
                ConvertTo-Json -Compress | Set-Content -LiteralPath ($OutputFile+'.refused.json')
            return $false
        }
        $bitmap=[Drawing.Bitmap]::new($rect.Right,$rect.Bottom)
        $graphics=[Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.CopyFromScreen($point.X,$point.Y,0,0,$bitmap.Size)
            # Recheck focus before retaining pixels; no full-desktop file is ever created.
            $after=[NgteamsSyntheticShot+Rect]::new()
            $afterPoint=[NgteamsSyntheticShot+Point]::new()
            if (![NgteamsSyntheticShot]::GetClientRect($handle,[ref]$after) -or
                ![NgteamsSyntheticShot]::ClientToScreen($handle,[ref]$afterPoint) -or
                $after.Right -ne $rect.Right -or $after.Bottom -ne $rect.Bottom -or
                $afterPoint.X -ne $point.X -or $afterPoint.Y -ne $point.Y -or
                ![NgteamsSyntheticShot]::CanCapture($handle,$point,$rect)) { return $false }
            $bitmap.Save($OutputFile,[Drawing.Imaging.ImageFormat]::Png)
        } finally { $graphics.Dispose(); $bitmap.Dispose() }
        return $true
    } finally { $null=[NgteamsSyntheticShot]::SetThreadDpiAwarenessContext($previousDpi) }
}
