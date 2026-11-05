param([Parameter(Mandatory)][string]$Setup)
# What an installer must do for a user, run for real: install, replace a running copy, uninstall a running copy.
# It works on the real install folder, so it belongs on a build server, or on a machine where that is the plan.
$ErrorActionPreference = 'Stop'
$dir = Join-Path $env:LOCALAPPDATA 'Programs\shotik'
$exe = Join-Path $dir 'Shotik.exe'

function Run-Setup([string]$File) {
  $p = Start-Process -FilePath $File -ArgumentList '/S' -PassThru -Wait
  if ($p.ExitCode -ne 0) { throw "$File /S exited with $($p.ExitCode)" }
}

function Start-Ghost {
  # Ghost mode and a private profile, so this needs no desktop and touches no real settings.
  $profile = Join-Path $env:TEMP "shotik-smoke-$([guid]::NewGuid().ToString('N'))"
  New-Item -ItemType Directory -Force (Join-Path $profile 'Shotik-Test') | Out-Null
  $env:APPDATA = $profile
  $env:SHOTIK_TEST = '1'
  $env:SHOTIK_GHOST = '1'
  $env:SHOTIK_PORT = '7691'
  $p = Start-Process -FilePath $exe -ArgumentList '--hidden', '--test' -PassThru
  Start-Sleep -Seconds 3
  if ($p.HasExited) { throw "the installed app exited at once with $($p.ExitCode)" }
  return $p
}

Run-Setup $Setup
foreach ($file in 'Shotik.exe', 'LICENSE', 'Uninstall Shotik.exe') {
  if (-not (Test-Path (Join-Path $dir $file))) { throw "the install did not write $file" }
}
$links = @(Get-ChildItem ([Environment]::GetFolderPath('Programs')) -Filter 'Shotik*.lnk').Count
if ($links -lt 7) { throw "the install made $links Start menu shortcuts of 7" }
'install: ok'

# A reinstall while the app runs must stop it and really replace the executable.
(Get-Item $exe).LastWriteTimeUtc = [datetime]'2000-01-01'
$running = Start-Ghost
Run-Setup $Setup
if (-not $running.HasExited) { $running.Kill(); throw 'the reinstall left the running copy alive' }
if ((Get-Item $exe).LastWriteTimeUtc.Year -eq 2000) { throw 'the reinstall did not replace the executable' }
'reinstall over a running copy: ok'

# So must an uninstall.
$running = Start-Ghost
$uninstaller = Join-Path $dir 'Uninstall Shotik.exe'
$p = Start-Process -FilePath $uninstaller -ArgumentList '/S' -PassThru -Wait
if ($p.ExitCode -ne 0) { throw "the uninstaller exited with $($p.ExitCode)" }
for ($i = 0; $i -lt 60 -and (Test-Path $exe); $i++) { Start-Sleep -Milliseconds 250 }
if (-not $running.HasExited) { $running.Kill(); throw 'the uninstall left the running copy alive' }
if (Test-Path $exe) { throw 'the uninstall left the executable behind' }
$left = @(Get-ChildItem ([Environment]::GetFolderPath('Programs')) -Filter 'Shotik*.lnk' -ErrorAction SilentlyContinue).Count
if ($left -ne 0) { throw "the uninstall left $left shortcuts" }
'uninstall of a running copy: ok'
