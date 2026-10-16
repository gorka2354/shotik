# Installed Shotik writes over the terminal UI (Windows)

Launching Shotik from an interactive terminal with `Start-Process ... --hidden`
can leave its output handles attached to that terminal. Chromium's native
DXGI diagnostics then appear over the terminal application's interface.
`--hidden` hides Shotik's window; it does not redirect its output.

In `src/main/capture.js`, `keepWarm()` requests a screen thumbnail every 30
seconds. If Windows desktop duplication fails, Chromium can emit native errors
on this schedule even though the JavaScript request is caught. This identifies
the source of terminal output, not the underlying reason for a DXGI failure.

For maintenance launches, quit the existing Shotik instance from its tray menu,
then run from the repository:

```powershell
node scripts/start-installed-windows.js
```

The launcher requires Node.js and the standard per-user Windows installation.
It detaches the process, redirects stdout/stderr and sends Chromium diagnostics
to timestamped files in `%LOCALAPPDATA%\Shotik\logs`. If another instance is
still running, Shotik's single-instance lock keeps that old process and its
original output handles; launching a second copy does not repair them.

This is for starting the installed application after local maintenance. Normal
launches through Explorer or the Start menu do not require Node.js.

Verified locally on 2026-09-09: the detached installed process retained its MCP
listener on `127.0.0.1:7464` after the launch shell exited, and a real region
screenshot succeeded. This does not establish the cause of the earlier Windows
DXGI errors or replace the separate microphone audibility check.

References: [Electron logging switches](https://www.electronjs.org/docs/latest/api/command-line-switches),
[Node.js detached processes](https://nodejs.org/api/child_process.html#optionsdetached).
