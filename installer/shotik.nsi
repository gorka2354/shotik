; Shotik installer: per-user, one click, the same location and shortcuts as the Electron version, so an
; upgrade keeps autostart, pinned shortcuts and Command Palette entries.
;
;   makensis /DVERSION=2.0.0 /DEXE=path\to\Shotik.exe /DOUT=path\to\Shotik-Setup-2.0.0.exe installer\shotik.nsi

Unicode true
SetCompressor /SOLID lzma
RequestExecutionLevel user
Name "Shotik"
OutFile "${OUT}"
InstallDir "$LOCALAPPDATA\Programs\shotik"
Icon "..\assets\icon.ico"
UninstallIcon "..\assets\icon.ico"
ShowInstDetails nevershow
ShowUninstDetails nevershow
AutoCloseWindow true
SilentUnInstall normal

!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\dev.shotik.app"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "Shotik"
VIAddVersionKey "FileDescription" "Shotik installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "MIT"

Page instfiles
UninstPage instfiles

!macro StopRunningShotik
  ; Only this install's own executable: nothing else named Shotik on the machine is touched. The path goes
  ; through the environment, because a folder name may hold a quote and a command line would break on it.
  ; It asks WMI, because NSIS is 32-bit, so this PowerShell is too, and it cannot read the path of a 64-bit process.
  System::Call 'Kernel32::SetEnvironmentVariable(t "SHOTIK_INSTDIR", t "$INSTDIR")'
  nsExec::Exec `powershell.exe -NoProfile -NonInteractive -Command "Get-CimInstance Win32_Process -Filter \"Name='Shotik.exe'\" | Where-Object { $$_.ExecutablePath -eq ($$env:SHOTIK_INSTDIR + '\Shotik.exe') } | ForEach-Object { Stop-Process -Id $$_.ProcessId -Force }; Start-Sleep -Milliseconds 500"`
  Pop $0
!macroend

Section "Shotik"
  SetShellVarContext current
  !insertmacro StopRunningShotik

  ; An Electron install leaves its uninstaller here: let it remove its own files and registry entry.
  ; It keeps user data. ExecWait returns once the uninstaller has finished, with _? keeping it in place.
  ; Our own uninstaller has the same name but must not run on a reinstall, because it removes autostart:
  ; only Electron has an app.asar.
  IfFileExists "$INSTDIR\resources\app.asar" 0 no_electron
    ExecWait `"$INSTDIR\Uninstall Shotik.exe" /S _?=$INSTDIR` $0
    Delete "$INSTDIR\Uninstall Shotik.exe"
    RMDir /r "$INSTDIR\resources"
    RMDir /r "$INSTDIR\locales"
    Delete "$INSTDIR\*.pak"
    Delete "$INSTDIR\*.dll"
    Delete "$INSTDIR\*.bin"
    Delete "$INSTDIR\*.dat"
    Delete "$INSTDIR\*.json"
    Delete "$INSTDIR\LICENSE*"
    Delete "$INSTDIR\version"
  no_electron:

  SetOutPath "$INSTDIR"
  SetOverwrite on
  ClearErrors
  File "/oname=Shotik.exe" "${EXE}"
  ; Silently keeping the old executable would look like a successful update.
  IfErrors 0 replaced
    MessageBox MB_ICONSTOP|MB_OK "Shotik.exe is in use and could not be replaced. Quit Shotik and run the installer again." /SD IDOK
    SetErrorLevel 2
    Abort
  replaced:
  File "/oname=LICENSE" "..\LICENSE"
  WriteUninstaller "$INSTDIR\Uninstall Shotik.exe"

  CreateShortcut "$SMPROGRAMS\Shotik.lnk" "$INSTDIR\Shotik.exe"
  CreateShortcut "$DESKTOP\Shotik.lnk" "$INSTDIR\Shotik.exe"
  ; PowerToys Command Palette and Win+S index Start Menu entries, so these become runnable commands.
  CreateShortcut "$SMPROGRAMS\Shotik Area Screenshot.lnk" "$INSTDIR\Shotik.exe" "--capture region" "$INSTDIR\Shotik.exe" 0
  CreateShortcut "$SMPROGRAMS\Shotik Full Screen.lnk" "$INSTDIR\Shotik.exe" "--capture full" "$INSTDIR\Shotik.exe" 0
  CreateShortcut "$SMPROGRAMS\Shotik Repeat Last Area.lnk" "$INSTDIR\Shotik.exe" "--capture repeat" "$INSTDIR\Shotik.exe" 0
  CreateShortcut "$SMPROGRAMS\Shotik Extract Text.lnk" "$INSTDIR\Shotik.exe" "--capture text" "$INSTDIR\Shotik.exe" 0
  CreateShortcut "$SMPROGRAMS\Shotik Translate Selection.lnk" "$INSTDIR\Shotik.exe" "--capture translate" "$INSTDIR\Shotik.exe" 0
  CreateShortcut "$SMPROGRAMS\Shotik Record Audio.lnk" "$INSTDIR\Shotik.exe" "--capture audio" "$INSTDIR\Shotik.exe" 0

  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "Shotik ${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "Shotik contributors"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\Shotik.exe,0"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" `"$INSTDIR\Uninstall Shotik.exe"`
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" `"$INSTDIR\Uninstall Shotik.exe" /S`
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1

  ; An interactive install opens the app, as the one-click Electron installer did. A silent one does not.
  IfSilent done
    Exec `"$INSTDIR\Shotik.exe"`
  done:
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  !insertmacro StopRunningShotik
  Delete "$SMPROGRAMS\Shotik.lnk"
  Delete "$DESKTOP\Shotik.lnk"
  Delete "$SMPROGRAMS\Shotik Area Screenshot.lnk"
  Delete "$SMPROGRAMS\Shotik Full Screen.lnk"
  Delete "$SMPROGRAMS\Shotik Repeat Last Area.lnk"
  Delete "$SMPROGRAMS\Shotik Extract Text.lnk"
  Delete "$SMPROGRAMS\Shotik Translate Selection.lnk"
  Delete "$SMPROGRAMS\Shotik Record Audio.lnk"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  DeleteRegValue HKCU "${RUN_KEY}" "dev.shotik.app"
  Delete "$INSTDIR\Shotik.exe"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\Uninstall Shotik.exe"
  RMDir "$INSTDIR"
  ; User data in %APPDATA%\Shotik and the screenshots themselves stay, as they did with Electron.
SectionEnd
