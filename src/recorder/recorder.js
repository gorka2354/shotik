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
const FORMAT = p.get('format') || 'auto';

const CANDIDATES = (FORMAT === 'mp4')
  ? ['video/mp4;codecs=h264', 'video/mp4', 'video/webm;codecs=vp9', 'video/webm']
  : ['video/webm;codecs=vp9', 'video/webm;codecs=vp8', 'video/webm'];

function pickMime() {
  for (const m of CANDIDATES) { if (window.MediaRecorder && MediaRecorder.isTypeSupported(m)) return m; }
  return 'video/webm';
}

let recorder, outCanvas, octx, srcVideo, rafId, synthTimer = null;
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

async function getSourceStream() {
  if (TEST) return synthStream();
  // main sets session.setDisplayMediaRequestHandler; this resolves to our display
  return await navigator.mediaDevices.getDisplayMedia({
    audio: AUDIO,
    video: { frameRate: FPS },
  });
}

// invoked by main via executeJavaScript('__shotikStart()', true)
window.__shotikStart = async function () {
  try {
    window.rec.trace('start; test=' + TEST + ' audio=' + AUDIO);
    const srcStream = await getSourceStream();
    window.rec.trace('got source stream, vtracks=' + srcStream.getVideoTracks().length);
    srcVideo = document.createElement('video');
    srcVideo.srcObject = srcStream; srcVideo.muted = true;
    await srcVideo.play();
    await new Promise((r) => (srcVideo.videoWidth ? r() : srcVideo.addEventListener('loadedmetadata', r, { once: true })));

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

    const outStream = outCanvas.captureStream(FPS);
    if (AUDIO) srcStream.getAudioTracks().forEach((t) => outStream.addTrack(t));

    const mime = pickMime();
    recorder = new MediaRecorder(outStream, { mimeType: mime, videoBitsPerSecond: 8_000_000 });
    recorder.ondataavailable = (e) => {
      if (!e.data || !e.data.size) return;
      sendChain = sendChain.then(async () => { window.rec.chunk(new Uint8Array(await e.data.arrayBuffer())); });
    };
    recorder.onstop = () => {
      const dur = (performance.now() - startedAt - pausedTotal) / 1000;
      sendChain.then(() => window.rec.done({ duration: Math.max(0, dur) }));
    };

    setTimeout(async () => {
      try {
        const blob = await new Promise((r) => outCanvas.toBlob(r, 'image/jpeg', 0.82));
        if (blob) window.rec.firstFrame(new Uint8Array(await blob.arrayBuffer()));
      } catch (_) {}
    }, 400);

    recorder.start(500);
    startedAt = performance.now();
    window.rec.trace('recorder.start ' + mime);
    window.rec.ready({ mime, ext: mime.startsWith('video/mp4') ? 'mp4' : 'webm' });
  } catch (e) {
    window.rec.trace('CAUGHT ' + String((e && e.message) || e));
    window.rec.error(String((e && e.message) || e));
  }
};

window.rec.onControl((cmd) => {
  if (cmd === 'stop') {
    try { if (recorder && recorder.state !== 'inactive') recorder.stop(); } catch (_) {}
    if (synthTimer) clearInterval(synthTimer);
    cancelAnimationFrame(rafId);
    if (srcVideo && srcVideo.srcObject) srcVideo.srcObject.getTracks().forEach((t) => t.stop());
  } else if (cmd === 'pause') {
    if (recorder && recorder.state === 'recording') { recorder.pause(); pauseStart = performance.now(); }
  } else if (cmd === 'resume') {
    if (recorder && recorder.state === 'paused') { recorder.resume(); pausedTotal += performance.now() - pauseStart; }
  }
});
