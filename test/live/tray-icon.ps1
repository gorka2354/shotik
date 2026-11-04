# Finds Shotik's icon where the shell draws it, on the taskbar or in the flyout of hidden icons, through UI
# Automation. Prints {"found":bool,"x":..,"y":..} in physical pixels: the middle of the icon.
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @"
using System; using System.Runtime.InteropServices;
public static class Dpi { [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr c); }
"@
[void][Dpi]::SetProcessDpiAwarenessContext([IntPtr]::new(-4))
$root = [System.Windows.Automation.AutomationElement]::RootElement
$name = [System.Windows.Automation.AutomationElement]::NameProperty
$found = $null
foreach ($class in 'TopLevelWindowForOverflowXamlIsland', 'XamlExplorerHostIslandWindow_WASDK', 'Shell_TrayWnd') {
  $cond = New-Object System.Windows.Automation.PropertyCondition ([System.Windows.Automation.AutomationElement]::ClassNameProperty), $class
  foreach ($window in $root.FindAll([System.Windows.Automation.TreeScope]::Children, $cond)) {
    foreach ($element in $window.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)) {
      $n = $element.GetCurrentPropertyValue($name)
      if ($n -and $n -like '*Shotik*') {
        $r = $element.Current.BoundingRectangle
        if ($r.Width -gt 0 -and $r.Width -lt 200) { $found = $r; break }
      }
    }
    if ($found) { break }
  }
  if ($found) { break }
}
if ($found) {
  '{{"found":true,"x":{0},"y":{1},"w":{2},"h":{3}}}' -f [int]($found.X + $found.Width / 2), [int]($found.Y + $found.Height / 2), [int]$found.Width, [int]$found.Height
} else {
  '{"found":false}'
}
