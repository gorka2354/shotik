# Microphone recording — 1.1.2

Screen recording previously requested only `getDisplayMedia()` loopback audio.
There was no microphone setting or `getUserMedia()` input, so a user's voice
could not enter the saved video unless already mixed into system playback.

## Behavior

- Settings → Screen recording has independent system-audio and microphone toggles.
- Microphone selection follows the system default or an exact selected device.
  Missing devices are shown as unavailable; recording does not silently switch inputs.
- Voice uses echo cancellation, noise suppression and automatic gain control.
- Two inputs are mixed through Web Audio into one MediaRecorder audio track,
  without monitoring them through the speakers. WebM and MP4 use Opus audio.
- Streams and audio contexts are released on stop, startup cancellation and errors.
  Microphone errors are explained in Russian and English.
- Microphone capture is off by default for existing and new installations;
  the owner explicitly enabled it locally while requesting this fix.
- GIF remains an image-only export.

## Validation on Windows, 2026-09-08

`npm test`: **76 passed, 0 failed**.

Unit tests cover device selection, denied access, missing/busy devices,
empty audio streams and stopping while microphone access is pending.
Ghost tests save real encoded videos with synthetic 440 Hz system audio and
880 Hz microphone input, then decode the files and measure both signals.
They cover silence, either input independently, both together, WebM and MP4,
plus persisted microphone settings and the unavailable-device UI.
Ghost tests never open real microphones or speakers. Test cleanup only stops
the processes spawned by its own harness.

`npx electron-builder --win nsis --publish never` built `Shotik-Setup-1.1.2.exe`.
An additional explicit local hardware check used the packaged application,
the default HyperX Quadcast microphone and Windows system audio. The resulting
video contains VP9 video and one 48 kHz stereo Opus audio track (checked with
`ffprobe` and decoded with `ffmpeg`). This verifies the capture path; no claim
is made that the owner spoke during the automated sample.

The local installed application was updated to 1.1.2, its package hash checked
against the tested build, and the owner's microphone toggle enabled with the
system-default device. The installer and hardware-check artifacts are in
ignored `dist/` and `out/`; nothing was published to GitHub.
