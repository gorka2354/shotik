# Eagerly snapshot every original format: IDataObject can otherwise be lazy.
# No window, keys, logging or serialization of the user's clipboard contents.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$original = [Windows.Forms.Clipboard]::GetDataObject()
$snapshot = New-Object Windows.Forms.DataObject
if ($null -ne $original) {
  foreach ($format in $original.GetFormats($false)) {
    $value = $original.GetData($format, $false)
    if ($value -is [Drawing.Image]) { $value = $value.Clone() }
    elseif ($value -is [IO.MemoryStream]) { $value = [IO.MemoryStream]::new($value.ToArray()) }
    if ($null -ne $value) { $snapshot.SetData($format, $false, $value) }
  }
}
[Console]::WriteLine('READY')
[Console]::In.ReadLine() | Out-Null
if ($snapshot.GetFormats($false).Length -gt 0) {
  [Windows.Forms.Clipboard]::SetDataObject($snapshot, $true, 10, 100)
} else { [Windows.Forms.Clipboard]::Clear() }
[Console]::WriteLine('RESTORED')
