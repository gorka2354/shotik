param([Parameter(Mandatory)][string]$Path, [double]$SeekTo = 0)
# Opens a file with the WinRT MediaPlayer (Media Foundation — the engine of the
# Windows "Media Player" app), muted and windowless, and reports what a player sees.
$ErrorActionPreference = 'Stop'
[void][Windows.Media.Playback.MediaPlayer, Windows.Media, ContentType = WindowsRuntime]
[void][Windows.Media.Core.MediaSource, Windows.Media.Core, ContentType = WindowsRuntime]
$p = [Windows.Media.Playback.MediaPlayer]::new()
$p.IsMuted = $true
$p.AutoPlay = $false
$p.Source = [Windows.Media.Core.MediaSource]::CreateFromUri([Uri]::new((Resolve-Path $Path).Path))
$s = $p.PlaybackSession
$t0 = Get-Date
while ($s.PlaybackState -eq 'Opening' -or $s.PlaybackState -eq 'None') {
  if (((Get-Date) - $t0).TotalSeconds -gt 20) { break }
  Start-Sleep -Milliseconds 100
}
$res = [ordered]@{ state = "$($s.PlaybackState)"; durationSec = $s.NaturalDuration.TotalSeconds; canSeek = $s.CanSeek }
if ($SeekTo -gt 0) {
  $s.Position = [TimeSpan]::FromSeconds($SeekTo)
  Start-Sleep -Milliseconds 1500
  $res.positionAfterSeek = $s.Position.TotalSeconds
}
$p.Dispose()
[pscustomobject]$res | ConvertTo-Json -Compress
