param([int]$X = 300, [int]$Y = 200, [int]$W = 900, [int]$H = 500, [string]$Title = 'Shotik live check', [int]$FontSize = 28)
# The window the live check selects text in and snaps to. It belongs to the check, not to the user.
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @"
using System; using System.Runtime.InteropServices;
public static class D { [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr c); [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n); }
"@
[void][D]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
$form = New-Object System.Windows.Forms.Form
$form.Text = $Title
$form.StartPosition = 'Manual'
$form.Bounds = New-Object System.Drawing.Rectangle $X, $Y, $W, $H
$form.TopMost = $true
$box = New-Object System.Windows.Forms.RichTextBox
$box.Dock = 'Fill'
$box.ReadOnly = $true
$box.Font = New-Object System.Drawing.Font 'Segoe UI', $FontSize
$box.Text = "Hello Shotik live selection test`nThe quick brown fox jumps over the lazy dog`nSecond paragraph with several plain words"
$form.Controls.Add($box)
# Started hidden, the process hands SW_HIDE to its first window: show it explicitly.
$null = $form.Handle
$form.Visible = $true
[void][D]::ShowWindow($form.Handle, 5)
[System.Windows.Forms.Application]::Run($form)
