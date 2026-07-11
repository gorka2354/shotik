# Background selection watcher for Shotik's auto-translate bubble.
# Passively READS the currently selected text via UI Automation and prints one
# JSON line whenever a stable text selection appears / changes / clears.
# It NEVER moves the mouse, clicks, or touches the clipboard — read-only.
# Arg: the parent (Shotik) PID — the watcher self-exits if that process is gone,
# so it never lingers as an orphan after a crash or forced kill.
param([int]$ParentPid = 0)
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class NativeSel {
  [DllImport("user32.dll")] public static extern short GetAsyncKeyState(int k);
  [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr c);
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X; public int Y; }
}
"@

# Report true physical pixels from GetCursorPos (per-monitor v2, fall back to
# system-DPI-aware) so the OCR-fallback region maps correctly on scaled displays.
try { [void][NativeSel]::SetProcessDpiAwarenessContext([IntPtr](-4)) } catch {}
try { [void][NativeSel]::SetProcessDPIAware() } catch {}

try {
  Add-Type -AssemblyName UIAutomationClient
  Add-Type -AssemblyName UIAutomationTypes
} catch { exit 1 }

$TextPatternId = [System.Windows.Automation.TextPattern]::Pattern

# Returns @{ Text; HasPattern }. HasPattern is true when the focused element
# exposes a UIA TextPattern (a normal text control). When it's FALSE the app is
# custom-drawn (Telegram posts, canvases) — that's when the OCR fallback applies.
function Get-Selection {
  try {
    $fe = [System.Windows.Automation.AutomationElement]::FocusedElement
    if ($null -eq $fe) { return @{ Text = ''; HasPattern = $false } }
    $tpObj = $null
    if (-not $fe.TryGetCurrentPattern($TextPatternId, [ref]$tpObj)) { return @{ Text = ''; HasPattern = $false } }
    $tp = [System.Windows.Automation.TextPattern]$tpObj
    $ranges = $tp.GetSelection()
    if ($null -eq $ranges -or $ranges.Length -eq 0) { return @{ Text = ''; HasPattern = $true } }
    $s = ''
    foreach ($r in $ranges) { $s += $r.GetText(5000) }
    return @{ Text = $s; HasPattern = $true }
  } catch { return @{ Text = ''; HasPattern = $false } }
}

function Write-Event($obj) {
  $json = $obj | ConvertTo-Json -Compress
  [Console]::Out.WriteLine($json)
  [Console]::Out.Flush()
}

# Prewarm UIA — the first FocusedElement call is ~100ms, the rest ~1-2ms.
[void](Get-Selection)

function Get-Cursor {
  $p = New-Object NativeSel+POINT
  [void][NativeSel]::GetCursorPos([ref]$p)
  return $p
}

$wasDown = $false
$downX = 0; $downY = 0  # where the left button went down (for drag detection)
$lastPressT = 0; $lastPressX = 0; $lastPressY = 0; $isDbl = $false
$lastEmitted = ''   # last UIA selection text we told the app about ('' = none/cleared)
$idle = 0
$tick = 0
$emptyStreak = 0    # consecutive empty reads — debounce so a transient empty read
                    # doesn't yank the bubble out from under a click

while ($true) {
  Start-Sleep -Milliseconds 55
  # Exit if the parent (Shotik) is gone, so we never linger as an orphan (~every 2s).
  $tick++
  if ($ParentPid -gt 0 -and ($tick % 36 -eq 0)) {
    try { $null = [System.Diagnostics.Process]::GetProcessById($ParentPid) } catch { exit 0 }
  }
  # High bit of GetAsyncKeyState (negative Int16) = button currently down.
  $down = ([NativeSel]::GetAsyncKeyState(0x01)) -lt 0
  $justPressed = (-not $wasDown) -and $down
  $justReleased = $wasDown -and (-not $down)
  $wasDown = $down
  if ($justPressed) {
    $d = Get-Cursor; $downX = $d.X; $downY = $d.Y
    # Report every press so the app can dismiss the bubble/popup on a click-away.
    Write-Event @{ down = 1; x = $d.X; y = $d.Y }
    # Double-click = two presses within 450ms at ~the same spot (selects a word).
    $nowT = [Environment]::TickCount
    $isDbl = (($nowT - $lastPressT) -lt 450) -and ([Math]::Abs($d.X - $lastPressX) -le 8) -and ([Math]::Abs($d.Y - $lastPressY) -le 8)
    $lastPressT = $nowT; $lastPressX = $d.X; $lastPressY = $d.Y
  }
  if ($down) { $idle = 0; continue }   # skip while the user is still dragging

  # Read right after a mouse release (snappy), or every ~330ms while idle
  # (to catch keyboard selection / Ctrl+A). Keeps UIA reads cheap when idle.
  $idle++
  if (-not $justReleased -and ($idle % 6 -ne 0)) { continue }

  $info = Get-Selection
  $sel = $info.Text
  if ($sel) { $sel = $sel.Trim() }

  if ([string]::IsNullOrEmpty($sel)) {
    # No UIA selection. If the focused element has NO TextPattern at all (a
    # custom-drawn app like Telegram posts), report the gesture so the app can
    # OCR that region: a horizontal-ish drag → the dragged span; a double-click →
    # the word at that point. Normal UIA apps have a TextPattern, so this never
    # fires there. Otherwise debounce a clear.
    if ($justReleased -and (-not $info.HasPattern)) {
      $up = Get-Cursor
      $dx = [Math]::Abs($up.X - $downX); $dy = [Math]::Abs($up.Y - $downY)
      if ($isDbl -and $dx -lt 25) {
        Write-Event @{ gesture = $true; dbl = $true; x1 = $up.X; y1 = $up.Y; x2 = $up.X; y2 = $up.Y }
        $isDbl = $false; $emptyStreak = 0; continue
      }
      if ($dx -ge 25 -and $dx -ge $dy) {   # selection drags run along a line, not vertical
        Write-Event @{ gesture = $true; x1 = $downX; y1 = $downY; x2 = $up.X; y2 = $up.Y }
        $emptyStreak = 0; continue
      }
    }
    $emptyStreak++
    if ($lastEmitted -ne '' -and $emptyStreak -ge 2) { Write-Event @{ clear = $true }; $lastEmitted = '' }
    continue
  }
  $emptyStreak = 0
  if ($sel -ne $lastEmitted) {
    $p = Get-Cursor
    $text = $sel
    if ($text.Length -gt 5000) { $text = $text.Substring(0, 5000) }
    Write-Event @{ x = $p.X; y = $p.Y; text = $text }
    $lastEmitted = $sel
  }
}
