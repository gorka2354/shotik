param([int]$ExcludePid = 0)
# Enumerate visible top-level windows with their DWM frame bounds (physical px,
# z-order top to bottom). Used by the overlay for hover-to-snap window selection.
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Collections.Generic;

public class ShotikWinEnum {
  delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hWnd);
  [DllImport("user32.dll")] static extern bool IsIconic(IntPtr hWnd);
  [DllImport("user32.dll")] static extern int GetWindowTextLength(IntPtr hWnd);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll")] static extern IntPtr SetProcessDpiAwarenessContext(IntPtr value);
  [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr h, int attr, out int v, int size);
  [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr h, int attr, out RECT r, int size);

  [StructLayout(LayoutKind.Sequential)]
  public struct RECT { public int L; public int T; public int R; public int B; }

  static List<string> rows = new List<string>();
  static uint skipPid;

  public static string Run(uint excludePid) {
    SetProcessDpiAwarenessContext(new IntPtr(-4)); // per-monitor v2: physical px
    skipPid = excludePid;
    EnumWindows(Cb, IntPtr.Zero);
    return "[" + string.Join(",", rows.ToArray()) + "]";
  }

  static bool Cb(IntPtr h, IntPtr lp) {
    if (!IsWindowVisible(h) || IsIconic(h)) return true;
    int cloaked = 0;
    DwmGetWindowAttribute(h, 14, out cloaked, 4); // DWMWA_CLOAKED
    if (cloaked != 0) return true;
    uint pid; GetWindowThreadProcessId(h, out pid);
    if (pid == skipPid) return true;
    if (GetWindowTextLength(h) == 0) return true;
    RECT r;
    if (DwmGetWindowAttribute(h, 9, out r, Marshal.SizeOf(typeof(RECT))) != 0) return true; // EXTENDED_FRAME_BOUNDS
    if (r.R - r.L < 48 || r.B - r.T < 48) return true;
    rows.Add("{\"x\":" + r.L + ",\"y\":" + r.T + ",\"w\":" + (r.R - r.L) + ",\"h\":" + (r.B - r.T) + "}");
    return true;
  }
}
"@

[ShotikWinEnum]::Run($ExcludePid)
