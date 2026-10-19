'use strict';
// What ends up in a recorded file, seen the way users open it: decoded audio
// (Chromium, in the ghost app's main window) and Media Foundation — the
// engine of Windows Media Player — via WinRT MediaPlayer (muted, no window).
const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');

// Amplitude of the 440 Hz ("system") and 880 Hz ("microphone") test tones
// in one second of the file's first channel, plus its decoded duration.
async function decodeAudio(app, file) {
  const data = fs.readFileSync(file).toString('base64');
  return app.post('exec-js', { target: 'main', code: `(async () => {
    const ctx = new AudioContext();
    try {
      const bytes = Uint8Array.from(atob(${JSON.stringify(data)}), c => c.charCodeAt(0));
      const audio = await ctx.decodeAudioData(bytes.buffer);
      const samples = audio.getChannelData(0);
      const start = Math.floor(audio.sampleRate * 0.2);
      const end = Math.min(samples.length, start + audio.sampleRate);
      const amplitude = frequency => {
        let real = 0, imaginary = 0;
        for (let i = start; i < end; i++) {
          const angle = 2 * Math.PI * frequency * i / audio.sampleRate;
          real += samples[i] * Math.cos(angle); imaginary += samples[i] * Math.sin(angle);
        }
        return 2 * Math.hypot(real, imaginary) / (end - start);
      };
      return { duration: audio.duration, system: amplitude(440), microphone: amplitude(880) };
    } finally { await ctx.close(); }
  })()` });
}

// { state, durationSec, canSeek, positionAfterSeek }
function mediaFoundation(file, seekTo) {
  const out = execFileSync('powershell.exe', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File',
    path.join(__dirname, 'mf-probe.ps1'), '-Path', file, '-SeekTo', String(seekTo)], { encoding: 'utf8', timeout: 60000 });
  return JSON.parse(out.trim().split(/\r?\n/).pop());
}

module.exports = { decodeAudio, mediaFoundation };
