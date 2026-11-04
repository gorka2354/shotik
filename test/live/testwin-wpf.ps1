param([int]$X = 300, [int]$Y = 200, [int]$W = 900, [int]$H = 400)
# A window whose text box speaks UI Automation's TextPattern, as browsers and modern editors do.
Add-Type -AssemblyName PresentationFramework
Add-Type -TypeDefinition @"
using System; using System.Runtime.InteropServices;
public static class D2 { [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n); }
"@
$box = New-Object System.Windows.Controls.TextBox
$box.Text = "Hello Shotik live selection test`nThe quick brown fox jumps over the lazy dog`nSecond paragraph with several plain words"
$box.FontSize = 18
$box.IsReadOnly = $true
$box.AcceptsReturn = $true
$window = New-Object System.Windows.Window
$window.Title = 'Shotik live check WPF'
$window.Left = $X
$window.Top = $Y
$window.Width = $W
$window.Height = $H
$window.Topmost = $true
$window.WindowStartupLocation = 'Manual'
$window.Content = $box
[void]$window.ShowDialog()
