'use strict';
/* Hidden capture window: grabs the display stream (getDisplayMedia via the main
   process's setDisplayMediaRequestHandler), crops it to the region on a canvas,
   and encodes with MediaRecorder. In TEST/GHOST mode the source is a synthetic
   animated canvas — the real desktop is never captured.

   Capture must be kicked off by main via executeJavaScript(..., true) so it runs
   with a user gesture (getDisplayMedia requires transient activation). */

const p = new URLSearchParams(location.search);
const TEST = p.get('test') === '1';
const PHYS = { w: +p.get('pw'), h: +p.get('ph') };            // full display, physical px
const CROP = { x: +p.get('x'), y: +p.get('y'), w: +p.get('w'), h: +p.get('h') }; // region, physical px
const FPS = +p.get('fps') || 30;
const AUDIO = p.get('audio') === '1';
const MICROPHONE = p.get('microphone') === '1';
const MICROPHONE_DEVICE = p.get('microphoneDeviceId') || 'default';
const FORMAT = p.get('format') || 'auto';

const WEBM_CANDIDATES = AUDIO || MICROPHONE
  ? ['video/webm;codecs=vp9,opus', 'video/webm;codecs=vp8,opus', 'video/webm']
  : ['video/webm;codecs=vp9', 'video/webm;codecs=vp8', 'video/webm'];
const CANDIDATES = FORMAT === 'mp4'
  ? [AUDIO || MICROPHONE ? 'video/mp4;codecs=avc1.42001E,opus' : 'video/mp4;codecs=h264', 'video/mp4', ...WEBM_CANDIDATES]
  : WEBM_CANDIDATES;

function pickMime() {
  for (const m of CANDIDATES) { if (window.MediaRecorder && MediaRecorder.isTypeSupported(m)) return m; }
  return 'video/webm';
}

let recorder, outCanvas, octx, srcVideo, rafId, synthTimer = null;
let srcStream, micStream, outStream, audioContext, previewTimer;
let stopping = false, starting = false;
const testAudioContexts = [];
let startedAt = 0, pausedTotal = 0, pauseStart = 0;
let sendChain = Promise.resolve();

function synthStream() {
  const c = document.createElement('canvas'); c.width = PHYS.w; c.height = PHYS.h;
  const cx = c.getContext('2d');
  let f = 0;
  synthTimer = setInterval(() => {
    f++;
    cx.fillStyle = '#12141c'; cx.fillRect(0, 0, c.width, c.height);
    const x = (f * 9) % Math.max(1, c.width - 260);
    cx.fillStyle = '#7c5cff'; cx.fillRect(x, 240, 220, 220);
    cx.fillStyle = '#fff'; cx.font = '52px sans-serif';
    cx.fillText('REC synthetic frame ' + f, 120, 140);
  }, 1000 / FPS);
  return c.captureStream(FPS);
}

// Ghost recordings use different tones for the two inputs, never real devices
// or speakers. Integration tests decode the saved file to verify both signals.
async function synthAudio(frequency) {
  const context = new AudioContext();
  testAudioContexts.push(context);
  const oscillator = context.createOscillator();
  oscillator.frequency.value = frequency;
  const gain = context.createGain(); gain.gain.value = 0.15;
  const destination = context.createMediaStreamDestination();
  oscillator.connect(gain).connect(destination);
  oscillator.start();
  await context.resume();
  return destination.stream;
}

async function getSourceStream() {
  if (TEST) {
    const stream = synthStream();
    if (AUDIO) (await synthAudio(440)).getAudioTracks().forEach((track) => stream.addTrack(track));
    return stream;
  }
  // main sets session.setDisplayMediaRequestHandler; this resolves to our display
  return await navigator.mediaDevices.getDisplayMedia({
    audio: AUDIO,
    video: { frameRate: FPS },
  });
}

async function getMicrophoneStream() {
  try {
    const stream = TEST ? await synthAudio(880) : await navigator.mediaDevices.getUserMedia({
      video: false,
      audio: {
        ...(MICROPHONE_DEVICE !== 'default' ? { deviceId: { exact: MICROPHONE_DEVICE } } : {}),
        echoCancellation: true, noiseSuppression: true, autoGainControl: true,
      },
    });
    if (!stream.getAudioTracks().length) {
      stream.getTracks().forEach((track) => track.stop());
      throw new Error('No audio track');
    }
    return stream;
  } catch (error) {
    throw new Error('microphone:' + (error.name || 'Error'));
  }
}

async function attachAudio() {
  const tracks = [
    ...(AUDIO ? srcStream.getAudioTracks() : []),
    ...(micStream ? micStream.getAudioTracks() : []),
  ];
  if (tracks.length === 1) outStream.addTrack(tracks[0]);
  else if (tracks.length > 1) {
    // MediaRecorder needs one mixed audio track. Adding two separate tracks
    // loses one input in some encoders/players. Do not monitor through speakers.
    audioContext = new AudioContext();
    const destination = audioContext.createMediaStreamDestination();
    for (const track of tracks) audioContext.createMediaStreamSource(new MediaStream([track])).connect(destination);
    destination.stream.getAudioTracks().forEach((track) => outStream.addTrack(track));
    await audioContext.resume();
  }
}

function releaseCapture() {
  if (synthTimer) clearInterval(synthTimer);
  clearTimeout(previewTimer);
  cancelAnimationFrame(rafId);
  const tracks = new Set([srcStream, micStream, outStream].filter(Boolean).flatMap((s) => s.getTracks()));
  tracks.forEach((track) => track.stop());
  if (srcVideo) { srcVideo.pause(); srcVideo.srcObject = null; }
  for (const context of [audioContext, ...testAudioContexts]) {
    if (context && context.state !== 'closed') context.close().catch(() => {});
  }
}

// invoked by main via executeJavaScript('__shotikStart()', true)
window.__shotikStart = async function () {
  if (starting || stopping) return;
  starting = true;
  try {
    window.rec.trace('start; test=' + TEST + ' audio=' + AUDIO + ' microphone=' + MICROPHONE);
    srcStream = await getSourceStream();
    if (stopping) { releaseCapture(); return; }
    if (MICROPHONE) micStream = await getMicrophoneStream();
    if (stopping) { releaseCapture(); return; }
    window.rec.trace('got source stream, vtracks=' + srcStream.getVideoTracks().length);
    srcVideo = document.createElement('video');
    srcVideo.srcObject = srcStream; srcVideo.muted = true;
    await srcVideo.play();
    await new Promise((r) => (srcVideo.videoWidth ? r() : srcVideo.addEventListener('loadedmetadata', r, { once: true })));
    if (stopping) { releaseCapture(); return; }

    // getDisplayMedia may hand back a different resolution than the display's
    // physical size — scale the crop rect to the actual track size.
    const sx = (srcVideo.videoWidth || PHYS.w) / PHYS.w;
    const sy = (srcVideo.videoHeight || PHYS.h) / PHYS.h;
    const crop = { x: CROP.x * sx, y: CROP.y * sy, w: CROP.w * sx, h: CROP.h * sy };

    outCanvas = document.createElement('canvas');
    outCanvas.width = CROP.w; outCanvas.height = CROP.h;
    octx = outCanvas.getContext('2d');
    const draw = () => {
      try { octx.drawImage(srcVideo, crop.x, crop.y, crop.w, crop.h, 0, 0, CROP.w, CROP.h); } catch (_) {}
      rafId = requestAnimationFrame(draw);
    };
    draw();

    outStream = outCanvas.captureStream(FPS);
    await attachAudio();
    if (stopping) { releaseCapture(); return; }

    const mime = pickMime();
    recorder = new MediaRecorder(outStream, { mimeType: mime, videoBitsPerSecond: 8_000_000 });
    recorder.ondataavailable = (e) => {
      if (!e.data || !e.data.size) return;
      sendChain = sendChain.then(async () => { window.rec.chunk(new Uint8Array(await e.data.arrayBuffer())); });
    };
    recorder.onstop = () => {
      const dur = (performance.now() - startedAt - pausedTotal) / 1000;
      releaseCapture();
      sendChain.then(() => window.rec.done({ duration: Math.max(0, dur) }));
    };
    recorder.onerror = (event) => {
      stopping = true;
      releaseCapture();
      window.rec.trace('MediaRecorder error: ' + String(event.error && event.error.message || 'Encoding failed'));
      window.rec.error(String(event.error && event.error.message || 'MediaRecorder failed'));
    };

    previewTimer = setTimeout(async () => {
      try {
        const blob = await new Promise((r) => outCanvas.toBlob(r, 'image/jpeg', 0.82));
        if (blob) window.rec.firstFrame(new Uint8Array(await blob.arrayBuffer()));
      } catch (_) {}
    }, 400);

    recorder.start(500);
    startedAt = performance.now();
    window.rec.trace('recorder.start ' + mime);
    window.rec.ready({ mime, ext: mime.startsWith('video/mp4') ? 'mp4' : 'webm',
      audio: { system: AUDIO && srcStream.getAudioTracks().length > 0, microphone: !!micStream, tracks: outStream.getAudioTracks().length },
    });
  } catch (e) {
    releaseCapture();
    if (stopping) return;
    stopping = true;
    window.rec.trace('CAUGHT ' + String((e && e.message) || e));
    window.rec.error(String((e && e.message) || e));
  }
};

window.rec.onControl((cmd) => {
  if (cmd === 'stop') {
    if (stopping) return;
    stopping = true;
    if (pauseStart) { pausedTotal += performance.now() - pauseStart; pauseStart = 0; }
    try { if (recorder && recorder.state !== 'inactive') recorder.stop(); } catch (_) {}
    releaseCapture();
    if (!startedAt) window.rec.done({ duration: 0, canceled: true });
  } else if (cmd === 'pause') {
    if (recorder && recorder.state === 'recording') { recorder.pause(); pauseStart = performance.now(); }
  } else if (cmd === 'resume') {
    if (recorder && recorder.state === 'paused') { recorder.resume(); pausedTotal += performance.now() - pauseStart; pauseStart = 0; }
  }
});
