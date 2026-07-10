'use strict';
/* Hidden capture window: grabs the display stream, crops it to the region on a
   canvas, and encodes with MediaRecorder. In TEST/GHOST mode the source is a
   synthetic animated canvas — the real desktop is never captured. */

const p = new URLSearchParams(location.search);
const TEST = p.get('test') === '1';
const SRC_ID = p.get('sourceId') || '';
const PHYS = { w: +p.get('pw'), h: +p.get('ph') };            // full display, physical px
const CROP = { x: +p.get('x'), y: +p.get('y'), w: +p.get('w'), h: +p.get('h') }; // region, physical px
const FPS = +p.get('fps') || 30;
const AUDIO = p.get('audio') === '1';
const FORMAT = p.get('format') || 'auto';                     // auto | webm | mp4

// 'auto' prefers WebM/VP9 — Chromium's MediaRecorder produces it correctly and
// with a matching extension. (mp4 output here is often VP9-in-mp4, which some
// players reject, so mp4 stays an explicit opt-in.)
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

async function getSourceStream() {
  if (TEST) {
    const c = document.createElement('canvas'); c.width = PHYS.w; c.height = PHYS.h;
    const cx = c.getContext('2d');
    let f = 0;
    synthTimer = setInterval(() => {
      f++;
      cx.fillStyle = '#12141c'; cx.fillRect(0, 0, c.width, c.height);
      const x = (f * 9) % Math.max(1, c.width - 260);
      cx.fillStyle = '#7c5cff'; cx.fillRect(x, 240, 220, 220);
      cx.fillStyle = '#3ddc84'; cx.beginPath(); cx.arc(c.width / 2, c.height / 2, 90, 0, 7); cx.fill();
      cx.fillStyle = '#fff'; cx.font = '52px sans-serif';
      cx.fillText('REC synthetic frame ' + f, 120, 140);
    }, 1000 / FPS);
    return c.captureStream(FPS);
  }
  return await navigator.mediaDevices.getUserMedia({
    audio: AUDIO ? { mandatory: { chromeMediaSource: 'desktop' } } : false,
    video: {
      mandatory: {
        chromeMediaSource: 'desktop', chromeMediaSourceId: SRC_ID,
        minWidth: PHYS.w, maxWidth: PHYS.w, minHeight: PHYS.h, maxHeight: PHYS.h, maxFrameRate: FPS,
      },
    },
  });
}

(async () => {
  try {
    const srcStream = await getSourceStream();
    srcVideo = document.createElement('video');
    srcVideo.srcObject = srcStream; srcVideo.muted = true;
    await srcVideo.play();

    outCanvas = document.createElement('canvas');
    outCanvas.width = CROP.w; outCanvas.height = CROP.h;
    octx = outCanvas.getContext('2d');

    const draw = () => {
      try { octx.drawImage(srcVideo, CROP.x, CROP.y, CROP.w, CROP.h, 0, 0, CROP.w, CROP.h); } catch (_) {}
      rafId = requestAnimationFrame(draw);
    };
    draw();

    const outStream = outCanvas.captureStream(FPS);
    if (AUDIO) srcStream.getAudioTracks().forEach((t) => outStream.addTrack(t));

    const mime = pickMime();
    recorder = new MediaRecorder(outStream, { mimeType: mime, videoBitsPerSecond: 8_000_000 });
    recorder.ondataavailable = (e) => {
      if (!e.data || !e.data.size) return;
      sendChain = sendChain.then(async () => {
        window.rec.chunk(new Uint8Array(await e.data.arrayBuffer()));
      });
    };
    recorder.onstop = () => {
      const dur = (performance.now() - startedAt - pausedTotal) / 1000;
      sendChain.then(() => window.rec.done({ duration: Math.max(0, dur) }));
    };

    // first frame → thumbnail (after the stream warms up)
    setTimeout(async () => {
      try {
        const blob = await new Promise((r) => outCanvas.toBlob(r, 'image/jpeg', 0.82));
        if (blob) window.rec.firstFrame(new Uint8Array(await blob.arrayBuffer()));
      } catch (_) {}
    }, 350);

    recorder.start(500);
    startedAt = performance.now();
    window.rec.ready({ mime, ext: mime.startsWith('video/mp4') ? 'mp4' : 'webm' });
  } catch (e) {
    window.rec.error(String((e && e.message) || e));
  }
})();

window.rec.onControl((cmd) => {
  if (cmd === 'stop') {
    try { if (recorder && recorder.state !== 'inactive') recorder.stop(); } catch (_) {}
    if (synthTimer) clearInterval(synthTimer);
    cancelAnimationFrame(rafId);
  } else if (cmd === 'pause') {
    if (recorder && recorder.state === 'recording') { recorder.pause(); pauseStart = performance.now(); }
  } else if (cmd === 'resume') {
    if (recorder && recorder.state === 'paused') { recorder.resume(); pausedTotal += performance.now() - pauseStart; }
  }
});
