# Audio in recordings

## Behaviour

- Settings → Screen recording and Settings → Audio recording have independent system-audio and
  microphone switches. The microphone is off by default.
- The microphone follows the system default or an exact selected device. A device that is gone is shown
  as unavailable: a recording never silently switches inputs.
- System audio is WASAPI loopback of the default output, the microphone is WASAPI capture. Both are
  resampled to 48 kHz, mixed into one track and encoded as AAC. Output devices at any rate work, 192 kHz included.
- The microphone asks Windows for the Communications category, so drivers that offer echo cancellation,
  noise suppression and gain control apply them. A driver that declines is not an error.
- If loopback is unavailable, a screen recording continues as video only and reports
  `audio:system-unavailable`. A missing or busy microphone is reported the same way.
- Devices are released when recording stops. GIF exports contain images only.

## Tests

The contract tests (`test/ghost/`) record real files with synthetic 440 Hz system audio and 880 Hz
microphone input, decode them through Media Foundation and measure both signals, for each input alone
and for both together. They never open real microphones or speakers.
