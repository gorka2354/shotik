param([string]$Title = 'ShotikCapTest')
# Off-screen (ghost-style) target window for the capture_window fallback test.
# NOTE: ShowInTaskbar stays TRUE — false makes it a WS_EX_TOOLWINDOW, which
# Chromium's window capturer excludes from its source list.
# NOTE: the parent spawns powershell hidden (SW_HIDE), and WinForms hands that
# startup show-state to the process's FIRST window — so the form must be
# force-shown via ShowWindow(SW_SHOWNOACTIVATE) or it never becomes visible.
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class ShotikSW { [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd); }
"@
$script:f = New-Object System.Windows.Forms.Form
$f.Text = $Title
$f.StartPosition = 'Manual'
$f.Location = New-Object System.Drawing.Point(22000, 300)   # off-screen (ghost-style)
$f.Size = New-Object System.Drawing.Size(400, 300)
$f.BackColor = [System.Drawing.Color]::FromArgb(200, 30, 40)  # red-ish
$p = New-Object System.Windows.Forms.Panel
$p.Location = New-Object System.Drawing.Point(40, 40)
$p.Size = New-Object System.Drawing.Size(120, 80)
$p.BackColor = [System.Drawing.Color]::FromArgb(30, 200, 90)  # green patch -> non-uniform image
$f.Controls.Add($p)
$script:t = New-Object System.Windows.Forms.Timer
$t.Interval = 250
$t.add_Tick({ [ShotikSW]::ShowWindow($script:f.Handle, 4) | Out-Null; $script:t.Stop() }) # SW_SHOWNOACTIVATE
$t.Start()
[System.Windows.Forms.Application]::Run($f)
