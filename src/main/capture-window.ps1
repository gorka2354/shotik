param([long]$Hwnd, [string]$OutFile)
# Capture ONE window's own surface via PrintWindow(PW_RENDERFULLCONTENT) — works
# even when the window is covered by others, off-screen or on another monitor
# (it renders the window's DWM-composited content, not the framebuffer).
# The bitmap is 24bpp on purpose: GDI blits leave the alpha channel at 0, which
# would otherwise produce a fully transparent PNG. The GetWindowRect capture is
# cropped to the DWM extended frame bounds to drop the invisible resize borders.
# Prints JSON {ok,w,h} on success; a thrown error surfaces on stderr.
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
Add-Type -AssemblyName System.Drawing

Add-Type @"
using System;
using System.Runtime.InteropServices;

public class ShotikWinCap {
  [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern IntPtr SetProcessDpiAwarenessContext(IntPtr value);
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int attr, out RECT r, int size);

  [StructLayout(LayoutKind.Sequential)]
  public struct RECT { public int L; public int T; public int R; public int B; }
}
"@

[void][ShotikWinCap]::SetProcessDpiAwarenessContext([IntPtr]-4) # per-monitor v2: physical px

$h = [IntPtr]$Hwnd
if (-not [ShotikWinCap]::IsWindow($h)) { throw "hwnd $Hwnd is not a window" }
if ([ShotikWinCap]::IsIconic($h)) { throw "window is minimized" }

$wr = New-Object ShotikWinCap+RECT
[void][ShotikWinCap]::GetWindowRect($h, [ref]$wr)
$w = $wr.R - $wr.L; $hh = $wr.B - $wr.T
if ($w -lt 1 -or $hh -lt 1) { throw "window has empty bounds ($w x $hh)" }

$bmp = New-Object System.Drawing.Bitmap($w, $hh, [System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
$ok = [ShotikWinCap]::PrintWindow($h, $hdc, 2) # PW_RENDERFULLCONTENT
$g.ReleaseHdc($hdc)
$g.Dispose()
if (-not $ok) { $bmp.Dispose(); throw "PrintWindow failed" }

# crop the invisible resize borders: GetWindowRect -> DWM extended frame bounds
$ef = New-Object ShotikWinCap+RECT
if ([ShotikWinCap]::DwmGetWindowAttribute($h, 9, [ref]$ef, [System.Runtime.InteropServices.Marshal]::SizeOf([type][ShotikWinCap+RECT])) -eq 0) {
  $cx = $ef.L - $wr.L; $cy = $ef.T - $wr.T
  $cw = $ef.R - $ef.L; $ch = $ef.B - $ef.T
  if ($cx -ge 0 -and $cy -ge 0 -and $cw -ge 1 -and $ch -ge 1 -and ($cx + $cw) -le $w -and ($cy + $ch) -le $hh -and ($cw -lt $w -or $ch -lt $hh)) {
    $rect = New-Object System.Drawing.Rectangle($cx, $cy, $cw, $ch)
    $cropped = $bmp.Clone($rect, $bmp.PixelFormat)
    $bmp.Dispose()
    $bmp = $cropped
  }
}

$bmp.Save($OutFile, [System.Drawing.Imaging.ImageFormat]::Png)
$outW = $bmp.Width; $outH = $bmp.Height
$bmp.Dispose()
Write-Output ("{""ok"":true,""w"":" + $outW + ",""h"":" + $outH + "}")
