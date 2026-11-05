# Persistent helper for the live check: reads one JSON command per line from stdin, answers with one JSON line.
# Input is only ever sent when the Node driver asks, and the driver checks the target window first.
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class W {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  [StructLayout(LayoutKind.Sequential)] struct LASTINPUTINFO { public uint cbSize; public uint dwTime; }
  [StructLayout(LayoutKind.Sequential)] struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr extra; }
  [StructLayout(LayoutKind.Sequential)] struct KEYBDINPUT { public ushort wVk, wScan; public uint dwFlags, time; public IntPtr extra; public int pad1, pad2; }
  [StructLayout(LayoutKind.Explicit)] struct INPUTUNION { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; }
  [StructLayout(LayoutKind.Sequential)] struct INPUT { public uint type; public INPUTUNION u; }
  [StructLayout(LayoutKind.Sequential)] public struct NOTIFYICONIDENTIFIER { public uint cbSize; public IntPtr hWnd; public uint uID; public Guid guidItem; }
  public delegate bool EnumProc(IntPtr h, IntPtr l);

  [DllImport("user32.dll")] static extern bool GetLastInputInfo(ref LASTINPUTINFO i);
  [DllImport("user32.dll")] static extern uint SendInput(uint n, INPUT[] inputs, int size);
  [DllImport("user32.dll")] static extern uint MapVirtualKey(uint code, uint map);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h, int i);
  [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT p);
  [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h, uint f);
  [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr h, uint c);
  [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
  [DllImport("user32.dll")] public static extern int GetSystemMetrics(int i);
  [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr c);
  [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowDisplayAffinity(IntPtr h, out uint a);
  [DllImport("user32.dll")] public static extern IntPtr GetKeyboardLayout(uint thread);
  [DllImport("user32.dll", SetLastError = true)] public static extern IntPtr SendMessageTimeout(IntPtr h, uint m, IntPtr w, IntPtr l, uint flags, uint timeout, out IntPtr result);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr parent, IntPtr after, string cls, string title);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern uint RegisterWindowMessage(string s);
  [DllImport("shell32.dll")] public static extern int Shell_NotifyIconGetRect(ref NOTIFYICONIDENTIFIER id, out RECT r);
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int size);
  [DllImport("dwmapi.dll", EntryPoint = "DwmGetWindowAttribute")] public static extern int DwmGetInt(IntPtr h, int a, out int v, int size);
  [DllImport("user32.dll")] public static extern int CountClipboardFormats();
  [DllImport("user32.dll")] public static extern bool OpenClipboard(IntPtr h);
  [DllImport("user32.dll")] public static extern bool CloseClipboard();
  [DllImport("user32.dll")] public static extern uint EnumClipboardFormats(uint f);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClipboardFormatName(uint f, StringBuilder s, int n);

  [DllImport("user32.dll")] static extern uint GetGuiResources(IntPtr process, uint flags);
  public static uint GuiResources(IntPtr process, uint flags) { return GetGuiResources(process, flags); }
  public static void PerMonitor() { SetThreadDpiAwarenessContext(new IntPtr(-4)); }

  public static long LastInputTick() { var i = new LASTINPUTINFO(); i.cbSize = (uint)Marshal.SizeOf(i); GetLastInputInfo(ref i); return i.dwTime; }
  public static long Tick() { return (uint)Environment.TickCount; }

  public static string ClassOf(IntPtr h) { var s = new StringBuilder(256); GetClassName(h, s, 256); return s.ToString(); }
  public static int TitleLen(IntPtr h) { var s = new StringBuilder(512); return GetWindowText(h, s, 512); }
  public static string TitleOf(IntPtr h) { var s = new StringBuilder(512); GetWindowText(h, s, 512); return s.ToString(); }
  public static uint PidOf(IntPtr h) { uint p; GetWindowThreadProcessId(h, out p); return p; }

  public static List<IntPtr> Top() {
    var list = new List<IntPtr>();
    EnumWindows((h, l) => { list.Add(h); return true; }, IntPtr.Zero);
    return list;
  }

  static void Send(INPUT[] inputs) { SendInput((uint)inputs.Length, inputs, Marshal.SizeOf(typeof(INPUT))); }

  static INPUT KeyInput(ushort vk, bool up) {
    var i = new INPUT(); i.type = 1; i.u.ki.wVk = vk; i.u.ki.wScan = (ushort)MapVirtualKey(vk, 0);
    uint flags = up ? 2u : 0u;
    // Extended keys: arrows, insert, delete, home, end, page up/down, snapshot, right ctrl/alt
    if ((vk >= 0x21 && vk <= 0x28) || vk == 0x2C || vk == 0x2D || vk == 0x2E) flags |= 1u;
    i.u.ki.dwFlags = flags; return i;
  }
  public static void Chord(int[] vks) {
    var list = new List<INPUT>();
    foreach (var v in vks) list.Add(KeyInput((ushort)v, false));
    for (int k = vks.Length - 1; k >= 0; k--) list.Add(KeyInput((ushort)vks[k], true));
    Send(list.ToArray());
  }
  public static void KeyState(int[] vks, bool up) {
    var list = new List<INPUT>();
    foreach (var v in vks) list.Add(KeyInput((ushort)v, up));
    Send(list.ToArray());
  }
  public static void TypeText(string text) {
    var list = new List<INPUT>();
    foreach (char c in text) {
      var d = new INPUT(); d.type = 1; d.u.ki.wScan = c; d.u.ki.dwFlags = 4; list.Add(d);
      var u = new INPUT(); u.type = 1; u.u.ki.wScan = c; u.u.ki.dwFlags = 4 | 2; list.Add(u);
    }
    Send(list.ToArray());
  }
  public static void MouseTo(int x, int y) {
    int vx = GetSystemMetrics(76), vy = GetSystemMetrics(77), vw = GetSystemMetrics(78), vh = GetSystemMetrics(79);
    var i = new INPUT(); i.type = 0;
    i.u.mi.dx = (int)Math.Round((x - vx) * 65535.0 / (vw - 1));
    i.u.mi.dy = (int)Math.Round((y - vy) * 65535.0 / (vh - 1));
    i.u.mi.dwFlags = 0x0001 | 0x8000 | 0x4000; // MOVE | ABSOLUTE | VIRTUALDESK
    Send(new[] { i });
  }
  public static void MouseButton(uint flags, uint data) {
    var i = new INPUT(); i.type = 0; i.u.mi.dwFlags = flags; i.u.mi.mouseData = data;
    Send(new[] { i });
  }
  public static string ClipFormats() {
    var names = new List<string>();
    if (!OpenClipboard(IntPtr.Zero)) return "busy";
    try {
      uint f = 0;
      while ((f = EnumClipboardFormats(f)) != 0) {
        var s = new StringBuilder(128);
        names.Add(GetClipboardFormatName(f, s, 128) > 0 ? s.ToString() : f.ToString());
      }
    } finally { CloseClipboard(); }
    return string.Join(",", names);
  }
}
"@ -ReferencedAssemblies System.Drawing

[W]::PerMonitor()
try { [Console]::InputEncoding = New-Object System.Text.UTF8Encoding($false); [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false) } catch {}

function WinInfo([IntPtr]$h) {
  $r = New-Object W+RECT; [void][W]::GetWindowRect($h, [ref]$r)
  $f = New-Object W+RECT
  $hasFrame = [W]::DwmGetWindowAttribute($h, 9, [ref]$f, 16) -eq 0
  $aff = [uint32]0; [void][W]::GetWindowDisplayAffinity($h, [ref]$aff)
  $cloaked = 0; [void][W]::DwmGetInt($h, 14, [ref]$cloaked, 4)
  $ex = [W]::GetWindowLong($h, -20)
  $style = [W]::GetWindowLong($h, -16)
  @{
    hwnd = $h.ToInt64(); pid = [W]::PidOf($h); cls = [W]::ClassOf($h); title = [W]::TitleOf($h)
    x = $r.L; y = $r.T; w = $r.R - $r.L; h = $r.B - $r.T
    frame = if ($hasFrame) { @{ x = $f.L; y = $f.T; w = $f.R - $f.L; h = $f.B - $f.T } } else { $null }
    visible = [W]::IsWindowVisible($h); dpi = [W]::GetDpiForWindow($h); affinity = $aff; cloaked = $cloaked
    topmost = ($ex -band 0x8) -ne 0; transparent = ($ex -band 0x20) -ne 0; noactivate = ($ex -band 0x08000000) -ne 0
    toolwindow = ($ex -band 0x80) -ne 0; layered = ($ex -band 0x80000) -ne 0
    caption = ($style -band 0xC00000) -eq 0xC00000; thickframe = ($style -band 0x40000) -ne 0
    owner = [W]::GetWindow($h, 4).ToInt64()
  }
}

function Handle($c) {
  switch ($c.op) {
    'tick' { return @{ tick = [W]::Tick(); lastInput = [W]::LastInputTick() } }
    'fg' {
      $h = [W]::GetForegroundWindow()
      if ($h -eq [IntPtr]::Zero) { return @{ hwnd = 0 } }
      $i = WinInfo $h
      $thread = [W]::GetWindowThreadProcessId($h, [ref]([uint32]0))
      $i.layout = ('{0:X8}' -f ([W]::GetKeyboardLayout($thread).ToInt64() -band 0xFFFFFFFF))
      return $i
    }
    'windows' {
      $out = @(); $z = 0
      foreach ($h in [W]::Top()) {
        $z++
        if ([W]::PidOf($h) -eq [uint32]$c.pid -and [W]::IsWindowVisible($h)) { $i = WinInfo $h; $i.z = $z; $out += $i }
      }
      return @{ windows = $out }
    }
    'at' {
      $p = New-Object W+POINT; $p.X = $c.x; $p.Y = $c.y
      $h = [W]::WindowFromPoint($p)
      if ($h -eq [IntPtr]::Zero) { return @{ hwnd = 0 } }
      $root = [W]::GetAncestor($h, 2)
      return (WinInfo $root)
    }
    'cursor' { $p = New-Object W+POINT; [void][W]::GetCursorPos([ref]$p); return @{ x = $p.X; y = $p.Y } }
    'monitors' {
      $out = @()
      foreach ($s in [System.Windows.Forms.Screen]::AllScreens) {
        $out += @{ x = $s.Bounds.X; y = $s.Bounds.Y; w = $s.Bounds.Width; h = $s.Bounds.Height; primary = $s.Primary
                   wx = $s.WorkingArea.X; wy = $s.WorkingArea.Y; ww = $s.WorkingArea.Width; wh = $s.WorkingArea.Height }
      }
      return @{ monitors = $out }
    }
    'key' { [W]::Chord([int[]]$c.vks); return @{ ok = $true; tick = [W]::Tick() } }
    'keydown' { [W]::KeyState([int[]]$c.vks, $false); return @{ ok = $true; tick = [W]::Tick() } }
    'keyup' { [W]::KeyState([int[]]$c.vks, $true); return @{ ok = $true; tick = [W]::Tick() } }
    'type' { [W]::TypeText($c.text); return @{ ok = $true; tick = [W]::Tick() } }
    'move' { [W]::MouseTo($c.x, $c.y); return @{ ok = $true; tick = [W]::Tick() } }
    'btn' { [W]::MouseButton([uint32]$c.flags, [uint32]$(if ($c.data) { $c.data } else { 0 })); return @{ ok = $true; tick = [W]::Tick() } }
    'hit' {
      $r = [IntPtr]::Zero
      $l = [IntPtr](($c.y -shl 16) -bor ($c.x -band 0xFFFF))
      $ok = [W]::SendMessageTimeout([IntPtr][long]$c.hwnd, 0x84, [IntPtr]::Zero, $l, 2, 2000, [ref]$r)
      return @{ sent = $ok -ne [IntPtr]::Zero; result = $r.ToInt64() }
    }
    'tray' {
      $h = [W]::FindWindowEx([IntPtr]::Zero, [IntPtr]::Zero, $c.cls, $null)
      $found = @()
      $after = [IntPtr]::Zero
      foreach ($parent in @([IntPtr]::new(-3))) {
        while ($true) {
          $after = [W]::FindWindowEx($parent, $after, $c.cls, $null)
          if ($after -eq [IntPtr]::Zero) { break }
          $found += $after
        }
      }
      $mine = $found | Where-Object { [W]::PidOf($_) -eq [uint32]$c.pid } | Select-Object -First 1
      if (-not $mine) { return @{ found = $false; count = $found.Count } }
      $id = New-Object W+NOTIFYICONIDENTIFIER
      $id.cbSize = [System.Runtime.InteropServices.Marshal]::SizeOf($id); $id.hWnd = $mine; $id.uID = 1
      $r = New-Object W+RECT
      $hr = [W]::Shell_NotifyIconGetRect([ref]$id, [ref]$r)
      return @{ found = $true; hwnd = $mine.ToInt64(); hr = $hr; x = $r.L; y = $r.T; w = $r.R - $r.L; h = $r.B - $r.T }
    }
    'post' { return @{ ok = [W]::PostMessage([IntPtr][long]$c.hwnd, [uint32]$c.msg, [IntPtr][long]$c.w, [IntPtr][long]$c.l) } }
    'regmsg' { return @{ id = [W]::RegisterWindowMessage($c.name) } }
    'shot' {
      $bmp = New-Object System.Drawing.Bitmap $c.w, $c.h
      $g = [System.Drawing.Graphics]::FromImage($bmp)
      $g.CopyFromScreen($c.x, $c.y, 0, 0, (New-Object System.Drawing.Size $c.w, $c.h))
      $g.Dispose()
      $out = $bmp
      if ($c.scale -and $c.scale -lt 1) {
        $w2 = [int]($c.w * $c.scale); $h2 = [int]($c.h * $c.scale)
        $out = New-Object System.Drawing.Bitmap $w2, $h2
        $g2 = [System.Drawing.Graphics]::FromImage($out)
        $g2.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $g2.DrawImage($bmp, 0, 0, $w2, $h2); $g2.Dispose(); $bmp.Dispose()
      }
      $out.Save($c.path, [System.Drawing.Imaging.ImageFormat]::Png); $out.Dispose()
      return @{ path = $c.path }
    }
    'clip' {
      $img = $null
      try { $img = [System.Windows.Forms.Clipboard]::GetImage() } catch {}
      $o = @{ formats = [W]::ClipFormats(); hasImage = $null -ne $img; text = $null }
      if ($img) { $o.w = $img.Width; $o.h = $img.Height; $img.Dispose() }
      try { if ([System.Windows.Forms.Clipboard]::ContainsText()) { $o.text = [System.Windows.Forms.Clipboard]::GetText() } } catch {}
      return $o
    }
    'proc' {
      $x = Get-Process -Id $c.pid
      return @{ handles = $x.HandleCount; privateMB = [math]::Round($x.PrivateMemorySize64 / 1MB, 1); threads = $x.Threads.Count;
                gdi = [W]::GuiResources($x.Handle, 0); user = [W]::GuiResources($x.Handle, 1) }
    }
    default { throw "unknown op $($c.op)" }
  }
}

[Console]::Out.WriteLine('{"ready":true}')
[Console]::Out.Flush()
while ($true) {
  $line = [Console]::In.ReadLine()
  if ($null -eq $line) { break }
  try {
    $cmd = $line | ConvertFrom-Json
    $result = Handle $cmd
    $json = @{ ok = $true; result = $result } | ConvertTo-Json -Depth 6 -Compress
  } catch {
    $json = @{ ok = $false; error = "$_" } | ConvertTo-Json -Compress
  }
  [Console]::Out.WriteLine($json)
  [Console]::Out.Flush()
}
