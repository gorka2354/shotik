# Renders the test/ocr-cases.json phrases to small PNGs (like Telegram post text
# / a selection highlight) so the OCR-for-selection path can be tested against a
# known ground truth. Texts come from a UTF-8 JSON so Cyrillic survives PS 5.1.
param([Parameter(Mandatory=$true)][string]$OutDir)
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
Add-Type -AssemblyName System.Drawing
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$cases = Get-Content (Join-Path $here '..\ocr-cases.json') -Encoding UTF8 -Raw | ConvertFrom-Json
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

function New-TextPng($text, $fontSize, $fg, $bg, $file) {
  $font = New-Object System.Drawing.Font('Segoe UI', $fontSize, [System.Drawing.FontStyle]::Regular)
  $t = New-Object System.Drawing.Bitmap 10, 10; $g0 = [System.Drawing.Graphics]::FromImage($t)
  $sz = $g0.MeasureString($text, $font); $g0.Dispose(); $t.Dispose()
  $w = [int]([Math]::Ceiling($sz.Width) + 16); $h = [int]([Math]::Ceiling($sz.Height) + 10)
  $bmp = New-Object System.Drawing.Bitmap $w, $h
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::ClearTypeGridFit
  $g.Clear($bg)
  $g.DrawString($text, $font, (New-Object System.Drawing.SolidBrush($fg)), 8, 4)
  $g.Dispose()
  $bmp.Save($file, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
}

foreach ($c in $cases) {
  $fg = if ($c.fg -like '#*') { [System.Drawing.ColorTranslator]::FromHtml($c.fg) } else { [System.Drawing.Color]::FromName($c.fg) }
  $bg = if ($c.bg -like '#*') { [System.Drawing.ColorTranslator]::FromHtml($c.bg) } else { [System.Drawing.Color]::FromName($c.bg) }
  New-TextPng $c.t ([single]$c.s) $fg $bg (Join-Path $OutDir "$($c.n).png")
}
Write-Host "ok $($cases.Count)"
