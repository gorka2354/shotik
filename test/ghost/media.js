'use strict';
// Decode the actual file without a browser. Media Foundation is the engine of
// Windows Media Player; the probe remains muted and creates no window.
const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');

function findMediaTool(name, env = process.env) {
  const exe = name + (process.platform === 'win32' ? '.exe' : '');
  const configured = env.FFMPEG_PATH;
  if (configured) {
    const candidate = fs.existsSync(configured) && fs.statSync(configured).isDirectory()
      ? path.join(configured, exe)
      : name === 'ffmpeg' ? configured : path.join(path.dirname(configured), exe);
    if (fs.existsSync(candidate) && fs.statSync(candidate).isFile()) return candidate;
  }
  for (const dir of (env.PATH || '').split(path.delimiter).filter(Boolean)) {
    const file = path.join(dir.replace(/^"|"$/g, ''), exe);
    if (fs.existsSync(file) && fs.statSync(file).isFile()) return file;
  }
  const packages = path.join(env.LOCALAPPDATA || '', 'Microsoft', 'WinGet', 'Packages');
  const inBin = (dir) => {
    const candidate = path.join(dir, 'bin', exe);
    if (fs.existsSync(candidate)) return candidate;
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      if (entry.isDirectory()) { const found = inBin(path.join(dir, entry.name)); if (found) return found; }
    }
    return null;
  };
  if (env.LOCALAPPDATA && fs.existsSync(packages)) {
    for (const entry of fs.readdirSync(packages, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
      if (entry.isDirectory() && entry.name.startsWith('Gyan.FFmpeg')) {
        const found = inBin(path.join(packages, entry.name));
        if (found) return found;
      }
    }
  }
  throw new Error(name + ' is required for ghost media tests. Set FFMPEG_PATH to ffmpeg.exe (with ffprobe.exe beside it), add it to PATH, or install Gyan.FFmpeg with WinGet.');
}

function probe(file, entries, streams) {
  const args = ['-v', 'error'];
  if (streams) args.push('-select_streams', streams);
  args.push('-show_entries', entries, '-of', 'json', file);
  return JSON.parse(execFileSync(findMediaTool('ffprobe'), args,
    { encoding: 'utf8', windowsHide: true, timeout: 30000 }));
}

function probeDuration(file) {
  const duration = Number(probe(file, 'format=duration').format?.duration);
  if (!Number.isFinite(duration) || duration <= 0) throw new Error('ffprobe found no finite positive duration: ' + file);
  return duration;
}

// Same first-channel Fourier amplitude and sample interval as the original
// AudioContext check: [0.2 s, 1.2 s), or as much as exists in a shorter file.
function toneAmplitudes(pcm, sampleRate) {
  const samples = pcm.length / 4;
  if (!Number.isInteger(samples) || sampleRate <= 0 || !Number.isFinite(sampleRate)) throw new Error('invalid f32le PCM');
  const start = Math.floor(sampleRate * 0.2);
  const end = Math.min(samples, start + sampleRate);
  if (end <= start) throw new Error('decoded audio is too short for the tone check');
  const amplitude = (frequency) => {
    let real = 0, imaginary = 0;
    for (let i = start; i < end; i++) {
      const angle = 2 * Math.PI * frequency * i / sampleRate;
      const sample = pcm.readFloatLE(i * 4);
      real += sample * Math.cos(angle); imaginary += sample * Math.sin(angle);
    }
    return 2 * Math.hypot(real, imaginary) / (end - start);
  };
  return { duration: samples / sampleRate, system: amplitude(440), microphone: amplitude(880) };
}

async function decodeAudio(file) {
  const sampleRate = Number(probe(file, 'stream=sample_rate', 'a:0').streams?.[0]?.sample_rate);
  if (!Number.isFinite(sampleRate) || sampleRate <= 0) throw new Error('ffprobe found no decodable audio stream: ' + file);
  const pcm = execFileSync(findMediaTool('ffmpeg'), ['-v', 'error', '-nostdin', '-threads', '2',
    '-i', file, '-map', '0:a:0', '-vn', '-af', 'pan=mono|c0=c0', '-ar', String(sampleRate),
    '-f', 'f32le', '-acodec', 'pcm_f32le', 'pipe:1'],
  { windowsHide: true, timeout: 60000, maxBuffer: 128 * 1024 * 1024 });
  return toneAmplitudes(pcm, sampleRate);
}

// { state, durationSec, canSeek, positionAfterSeek }
function mediaFoundation(file, seekTo) {
  const out = execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File',
    path.join(__dirname, 'mf-probe.ps1'), '-Path', file, '-SeekTo', String(seekTo)],
  { encoding: 'utf8', windowsHide: true, timeout: 60000 });
  return JSON.parse(out.trim().split(/\r?\n/).pop());
}

module.exports = { decodeAudio, mediaFoundation, probeDuration, toneAmplitudes, findMediaTool };
