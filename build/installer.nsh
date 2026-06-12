; Extra Start Menu shortcuts with CLI arguments.
; PowerToys Command Palette (and Win+S search) index Start Menu entries,
; so these become runnable commands: type "shotik" in the palette.
!macro customInstall
  CreateShortcut "$SMPROGRAMS\Shotik Area Screenshot.lnk" "$INSTDIR\Shotik.exe" "--capture region" "$INSTDIR\Shotik.exe" 0
  CreateShortcut "$SMPROGRAMS\Shotik Full Screen.lnk" "$INSTDIR\Shotik.exe" "--capture full" "$INSTDIR\Shotik.exe" 0
  CreateShortcut "$SMPROGRAMS\Shotik Repeat Last Area.lnk" "$INSTDIR\Shotik.exe" "--capture repeat" "$INSTDIR\Shotik.exe" 0
!macroend

!macro customUnInstall
  Delete "$SMPROGRAMS\Shotik Area Screenshot.lnk"
  Delete "$SMPROGRAMS\Shotik Full Screen.lnk"
  Delete "$SMPROGRAMS\Shotik Repeat Last Area.lnk"
!macroend
