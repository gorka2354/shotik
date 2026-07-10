# Sends Ctrl+C to the currently focused window to copy its selection.
# Used by the "translate selected text" feature (no screenshot needed).
$ErrorActionPreference = 'SilentlyContinue'
Add-Type -AssemblyName System.Windows.Forms
[System.Windows.Forms.SendKeys]::SendWait('^c')
Start-Sleep -Milliseconds 90
