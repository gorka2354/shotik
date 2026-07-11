# Renders a line of text with ONE sub-span highlighted (coloured background),
# like a partial selection in Telegram/Steam. Prints JSON {sx,sy,word} — the
# highlight's centre (image px) and the highlighted word — so the highlight-crop
# OCR can be tested against ground truth.
param([Parameter(Mandatory=$true)][string]$OutFile)
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
Add-Type -AssemblyName System.Drawing

$pre = 'meet me at the '
$word = 'STATION'
$post = ' tomorrow please'
$all = $pre + $word + $post
$fontSize = 12
$font = New-Object System.Drawing.Font('Segoe UI', $fontSize, [System.Drawing.FontStyle]::Regular)

$t = New-Object System.Drawing.Bitmap 10,10; $g0 = [System.Drawing.Graphics]::FromImage($t)
$fmt = [System.Drawing.StringFormat]::GenericTypographic
$allSz = $g0.MeasureString($all, $font, 10000, $fmt)
$preSz = $g0.MeasureString($pre, $font, 10000, $fmt)
$wordSz = $g0.MeasureString($word, $font, 10000, $fmt)
$g0.Dispose(); $t.Dispose()

$padL = 10; $padT = 6
$w = [int]([Math]::Ceiling($allSz.Width) + $padL * 2)
$h = [int]([Math]::Ceiling($allSz.Height) + $padT * 2)
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::ClearTypeGridFit
$g.Clear([System.Drawing.Color]::White)

# highlight rectangle behind just the word; the selected word is drawn white on
# it (like a real selection), the rest black on white
$hlX = $padL + $preSz.Width
$hlColor = [System.Drawing.ColorTranslator]::FromHtml('#3390EC')
$g.FillRectangle((New-Object System.Drawing.SolidBrush($hlColor)), $hlX, $padT, $wordSz.Width, $allSz.Height)
$g.DrawString($pre, $font, [System.Drawing.Brushes]::Black, $padL, $padT, $fmt)
$g.DrawString($word, $font, [System.Drawing.Brushes]::White, $hlX, $padT, $fmt)
$g.DrawString($post, $font, [System.Drawing.Brushes]::Black, ($hlX + $wordSz.Width), $padT, $fmt)
$g.Dispose()
$bmp.Save($OutFile, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()

$sx = [int]($hlX + $wordSz.Width / 2)
$sy = [int]($padT + $allSz.Height / 2)
Write-Output (@{ sx = $sx; sy = $sy; word = $word } | ConvertTo-Json -Compress)
