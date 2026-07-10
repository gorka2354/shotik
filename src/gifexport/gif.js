'use strict';
/* Decodes a recorded video and samples frames to RGBA, streaming them to main
   which encodes the GIF. Playing the video + requestVideoFrameCallback gives
   reliably-decoded frames (seeking VP9 WebM often draws blank frames). */

const p = new URLSearchParams(location.search);
const SRC = p.get('src');
const FPS = +p.get('fps') || 12;
const MAXW = +p.get('maxw') || 640;
const MAX_FRAMES = +p.get('maxframes') || 400;

(async () => {
  try {
    const v = document.createElement('video');
    v.muted = true; v.src = SRC;
    await new Promise((res, rej) => {
      v.addEventListener('loadedmetadata', res, { once: true });
      v.addEventListener('error', () => rej(new Error('cannot load video')), { once: true });
    });

    const scale = Math.min(1, MAXW / (v.videoWidth || MAXW));
    const w = Math.max(1, Math.round(v.videoWidth * scale));
    const h = Math.max(1, Math.round(v.videoHeight * scale));
    const c = document.createElement('canvas'); c.width = w; c.height = h;
    const cx = c.getContext('2d', { willReadFrequently: true });
    window.gif.meta({ w, h, fps: FPS, delay: Math.round(1000 / FPS) });

    const minGap = 1 / FPS - 0.004;
    let lastT = -1, count = 0, finished = false;
    const finish = () => { if (finished) return; finished = true; window.gif.done(); };

    if (!v.requestVideoFrameCallback) {
      // fallback: timed sampling during playback
      v.playbackRate = 1;
      await v.play();
      const iv = setInterval(() => {
        if (v.ended || count >= MAX_FRAMES) { clearInterval(iv); finish(); return; }
        cx.drawImage(v, 0, 0, w, h);
        window.gif.frame(new Uint8Array(cx.getImageData(0, 0, w, h).data.buffer.slice(0)));
        count++;
      }, 1000 / FPS);
      return;
    }

    const onFrame = (now, meta) => {
      const t = meta.mediaTime;
      if (t - lastT >= minGap && count < MAX_FRAMES) {
        lastT = t;
        cx.drawImage(v, 0, 0, w, h);
        window.gif.frame(new Uint8Array(cx.getImageData(0, 0, w, h).data.buffer.slice(0)));
        count++;
      }
      if (v.ended || count >= MAX_FRAMES) finish();
      else v.requestVideoFrameCallback(onFrame);
    };
    v.addEventListener('ended', finish, { once: true });
    // speed up export for long clips without dropping sampled frames
    v.playbackRate = Math.min(4, Math.max(1, (v.duration && isFinite(v.duration) ? v.duration : 6) / 12));
    v.requestVideoFrameCallback(onFrame);
    await v.play();
  } catch (e) {
    window.gif.error(String((e && e.message) || e));
  }
})();
