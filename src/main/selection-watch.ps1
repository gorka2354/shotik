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
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X; public int Y; }
}
"@

try {
  Add-Type -AssemblyName UIAutomationClient
  Add-Type -AssemblyName UIAutomationTypes
} catch { exit 1 }

$TextPatternId = [System.Windows.Automation.TextPattern]::Pattern

function Get-SelectionText {
  try {
    $fe = [System.Windows.Automation.AutomationElement]::FocusedElement
    if ($null -eq $fe) { return '' }
    $tpObj = $null
    if (-not $fe.TryGetCurrentPattern($TextPatternId, [ref]$tpObj)) { return '' }
    $tp = [System.Windows.Automation.TextPattern]$tpObj
    $ranges = $tp.GetSelection()
    if ($null -eq $ranges -or $ranges.Length -eq 0) { return '' }
    $s = ''
    foreach ($r in $ranges) { $s += $r.GetText(5000) }
    return $s
  } catch { return '' }
}

function Write-Event($obj) {
  $json = $obj | ConvertTo-Json -Compress
  [Console]::Out.WriteLine($json)
  [Console]::Out.Flush()
}

# Prewarm UIA — the first FocusedElement call is ~100ms, the rest ~1-2ms.
[void](Get-SelectionText)

$wasDown = $false
$lastEmitted = ''   # last selection text we told the app about ('' = none/cleared)
$idle = 0
$tick = 0

while ($true) {
  Start-Sleep -Milliseconds 90
  # Exit if the parent (Shotik) is gone, so we never linger as an orphan (~every 2s).
  $tick++
  if ($ParentPid -gt 0 -and ($tick % 22 -eq 0)) {
    try { $null = [System.Diagnostics.Process]::GetProcessById($ParentPid) } catch { exit 0 }
  }
  # High bit of GetAsyncKeyState (negative Int16) = button currently down.
  $down = ([NativeSel]::GetAsyncKeyState(0x01)) -lt 0
  $justReleased = $wasDown -and (-not $down)
  $wasDown = $down
  if ($down) { $idle = 0; continue }   # skip while the user is still dragging

  # Read right after a mouse release (snappy), or every ~540ms while idle
  # (to catch keyboard selection / Ctrl+A). Keeps UIA reads cheap when idle.
  $idle++
  if (-not $justReleased -and ($idle % 6 -ne 0)) { continue }

  $sel = Get-SelectionText
  if ($sel) { $sel = $sel.Trim() }

  if ([string]::IsNullOrEmpty($sel)) {
    if ($lastEmitted -ne '') { Write-Event @{ clear = $true }; $lastEmitted = '' }
    continue
  }
  if ($sel -ne $lastEmitted) {
    $p = New-Object NativeSel+POINT
    [void][NativeSel]::GetCursorPos([ref]$p)
    $text = $sel
    if ($text.Length -gt 5000) { $text = $text.Substring(0, 5000) }
    Write-Event @{ x = $p.X; y = $p.Y; text = $text }
    $lastEmitted = $sel
  }
}
