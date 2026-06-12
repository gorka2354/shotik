# Publishing Shotik to the Microsoft Store

The MSIX package is built with `npm run dist:store` (requires the Windows 10/11 SDK,
electron-builder `appx` target). The Store signs packages itself, so **no code-signing
certificate is needed** and SmartScreen warnings disappear for Store installs.

## One-time setup (account owner)

1. Register a [Partner Center](https://partner.microsoft.com/dashboard/registration) developer
   account (individual, one-time fee ≈ $19).
2. Create a new app and **reserve the name** “Shotik”.
3. Open *Product identity* and copy three values:
   - `Package/Identity/Name` → put into `build.appx.identityName` in `package.json`
   - `Package/Identity/Publisher` (a `CN=GUID` string) → `build.appx.publisher`
   - `Package/Properties/PublisherDisplayName` → `build.appx.publisherDisplayName`
4. `npm run dist:store` → upload `dist/Shotik-x.x.x.appx` in a new submission.
5. Fill the listing (screenshots from `docs/`, the demo GIF re-encoded as MP4 works well),
   set age ratings, submit for certification (usually 1–3 days).

## Notes

- The MSIX runs in a light container: settings/history live in the same `%APPDATA%\Shotik`,
  global hotkeys and the tray work as usual.
- "Start with Windows" inside MSIX uses the Store's StartupTask mechanism — verify it after
  the first certification pass; if it misbehaves, gate `app.setLoginItemSettings` behind
  `process.windowsStore`.
- Store builds auto-update through the Store, so point users there once it's live.
