param([int]$ExcludePid = 0, [switch]$Detailed)
# Enumerate visible top-level windows with their DWM frame bounds (physical px,
# z-order top to bottom). Fast mode (default): bare rects for the overlay's
# hover-to-snap. -Detailed: hwnd/title/pid/process/minimized for the MCP
# list_windows / capture_window tools (includes minimized windows, flagged).
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

Add-Type @"
using System;
using System.Text;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Collections.Generic;

public class ShotikWinEnum {
  delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hWnd);
  [DllImport("user32.dll")] static extern bool IsIconic(IntPtr hWnd);
  [DllImport("user32.dll")] static extern int GetWindowTextLength(IntPtr hWnd);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr hWnd, StringBuilder sb, int max);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll")] static extern bool GetWindowPlacement(IntPtr hWnd, ref WINDOWPLACEMENT p);
  [DllImport("user32.dll")] static extern IntPtr SetProcessDpiAwarenessContext(IntPtr value);
  [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr h, int attr, out int v, int size);
  [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr h, int attr, out RECT r, int size);

  [StructLayout(LayoutKind.Sequential)]
  public struct RECT { public int L; public int T; public int R; public int B; }
  [StructLayout(LayoutKind.Sequential)]
  public struct POINT { public int X; public int Y; }
  [StructLayout(LayoutKind.Sequential)]
  public struct WINDOWPLACEMENT {
    public int length; public int flags; public int showCmd;
    public POINT minPos; public POINT maxPos; public RECT normalPos;
  }

  static List<string> rows = new List<string>();
  static uint skipPid;
  static bool detailed;

  public static string Run(uint excludePid, bool wantDetailed) {
    SetProcessDpiAwarenessContext(new IntPtr(-4)); // per-monitor v2: physical px
    skipPid = excludePid;
    detailed = wantDetailed;
    EnumWindows(Cb, IntPtr.Zero);
    return "[" + string.Join(",", rows.ToArray()) + "]";
  }

  // JSON string escape (titles can contain quotes/backslashes/control chars)
  static string J(string s) {
    var sb = new StringBuilder("\"");
    foreach (char c in s) {
      if (c == '"') sb.Append("\\\"");
      else if (c == '\\') sb.Append("\\\\");
      else if (c < ' ') sb.Append("\\u").Append(((int)c).ToString("x4"));
      else sb.Append(c);
    }
    return sb.Append("\"").ToString();
  }

  static bool Cb(IntPtr h, IntPtr lp) {
    if (!IsWindowVisible(h)) return true;
    bool min = IsIconic(h);
    if (min && !detailed) return true; // hover-snap only cares about on-screen windows
    int cloaked = 0;
    DwmGetWindowAttribute(h, 14, out cloaked, 4); // DWMWA_CLOAKED
    if (cloaked != 0) return true;
    uint pid; GetWindowThreadProcessId(h, out pid);
    if (pid == skipPid) return true;
    int tl = GetWindowTextLength(h);
    if (tl == 0) return true;

    RECT r;
    if (min) {
      // DWM bounds of an iconic window point at -32000; report the restored rect
      var wp = new WINDOWPLACEMENT(); wp.length = Marshal.SizeOf(typeof(WINDOWPLACEMENT));
      if (!GetWindowPlacement(h, ref wp)) return true;
      r = wp.normalPos;
    } else {
      if (DwmGetWindowAttribute(h, 9, out r, Marshal.SizeOf(typeof(RECT))) != 0) return true; // EXTENDED_FRAME_BOUNDS
    }
    if (r.R - r.L < 48 || r.B - r.T < 48) return true;

    if (!detailed) {
      rows.Add("{\"x\":" + r.L + ",\"y\":" + r.T + ",\"w\":" + (r.R - r.L) + ",\"h\":" + (r.B - r.T) + "}");
      return true;
    }
    var sb2 = new StringBuilder(tl + 2);
    GetWindowText(h, sb2, tl + 2);
    string app = "";
    try { app = Process.GetProcessById((int)pid).ProcessName; } catch (Exception) {}
    rows.Add("{\"hwnd\":" + h.ToInt64() + ",\"title\":" + J(sb2.ToString()) + ",\"pid\":" + pid +
      ",\"app\":" + J(app) + ",\"x\":" + r.L + ",\"y\":" + r.T + ",\"w\":" + (r.R - r.L) +
      ",\"h\":" + (r.B - r.T) + ",\"min\":" + (min ? "true" : "false") + "}");
    return true;
  }
}
"@

[ShotikWinEnum]::Run($ExcludePid, [bool]$Detailed)
