//! A video to an animated GIF: frames sampled at a fixed rate, scaled, one palette for all of them.
//! Reference: `src/main/gifexport.js` and `src/gifexport/gif.js`, with the palette algorithm of
//! gifenc (pairwise nearest neighbour over a 5-bit-per-channel histogram).
//!
//! Frames are kept as histogram keys rather than RGBA, so the longest export of 400 frames at
//! 640x360 needs 184 MB, not 368. The histogram covers every frame instead of a sample of 12, which
//! costs little and cannot miss a colour that only shows briefly.

use std::borrow::Cow;
use std::fs::{self, File};
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};

use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use shotik_core::{Error, Result};

const DEFAULT_FPS: u32 = 12;
const DEFAULT_MAX_WIDTH: u32 = 640;
/// `MAX_FRAMES` of gif.js.
const MAX_FRAMES: usize = 400;
const KEYS: usize = 65536;
/// The sampling tolerance of gif.js.
const GAP_TOLERANCE_SEC: f64 = 0.004;

/// Writes `<video without extension>.gif` next to the video and returns its path. Zero for `fps` or
/// `max_width` means the defaults of the JS version, 12 and 640.
#[cfg(windows)]
pub fn export_gif(video: &Path, fps: u32, max_width: u32) -> Result<PathBuf> {
    let fps = if fps == 0 { DEFAULT_FPS } else { fps };
    let max_width = if max_width == 0 { DEFAULT_MAX_WIDTH } else { max_width };
    let mut gif = GifBuilder::new(fps, max_width);
    crate::win::gif_decode::decode_frames(video, &mut gif)?;
    if gif.is_empty() {
        return Err(Error::msg(format!("No frames decoded from {}", video.display())));
    }
    let out = video.with_extension("gif");
    gif.finish(&out)?;
    Ok(out)
}

/// Writes `<video without extension>.gif` next to the video.
#[cfg(not(windows))]
pub fn export_gif(_video: &Path, _fps: u32, _max_width: u32) -> Result<PathBuf> {
    Err(Error::Unsupported("GIF export"))
}

/// The packing of gifenc's `rgb888_to_rgb565`, quirk included: effectively 5 bits per channel.
fn key_of(r: u8, g: u8, b: u8) -> usize {
    ((usize::from(r) << 8) & 0xf800) | ((usize::from(g) << 2) & 0x03e0) | (usize::from(b) >> 3)
}

struct Histogram {
    r: Vec<u64>,
    g: Vec<u64>,
    b: Vec<u64>,
    count: Vec<u64>,
}

impl Histogram {
    fn new() -> Self {
        Self { r: vec![0; KEYS], g: vec![0; KEYS], b: vec![0; KEYS], count: vec![0; KEYS] }
    }

    fn mean(&self, key: usize) -> Option<(f64, f64, f64)> {
        let n = self.count[key];
        (n > 0).then(|| {
            let n = n as f64;
            (self.r[key] as f64 / n, self.g[key] as f64 / n, self.b[key] as f64 / n)
        })
    }
}

#[derive(Clone, Copy, Default)]
struct Bin {
    r: f64,
    g: f64,
    b: f64,
    count: f64,
    nn: usize,
    fw: usize,
    bk: usize,
    tm: u32,
    mtm: u32,
    err: f64,
}

fn find_nn(bins: &mut [Bin], idx: usize) {
    let mut nn = 0;
    let mut err = 1e100;
    let bin1 = bins[idx];
    let mut i = bin1.fw;
    while i != 0 {
        let bin = &bins[i];
        let next = bin.fw;
        let weight = (bin1.count * bin.count) / (bin1.count + bin.count);
        let mut nerr = 0.0;
        if weight < err {
            nerr += weight * (bin.r - bin1.r).powi(2);
            if nerr < err {
                nerr += weight * (bin.g - bin1.g).powi(2);
                if nerr < err {
                    nerr += weight * (bin.b - bin1.b).powi(2);
                    if nerr < err {
                        err = nerr;
                        nn = i;
                    }
                }
            }
        }
        i = next;
    }
    bins[idx].err = err;
    bins[idx].nn = nn;
}

/// `quantize` of gifenc's pnnquant2.js for RGB without alpha.
fn quantize(hist: &Histogram, max_colors: usize) -> Vec<[u8; 3]> {
    let mut bins: Vec<Bin> = (0..KEYS)
        .filter_map(|key| {
            hist.mean(key).map(|(r, g, b)| Bin { r, g, b, count: hist.count[key] as f64, ..Bin::default() })
        })
        .collect();
    let max_bins = bins.len();
    if max_bins == 0 {
        return Vec::new();
    }
    let use_sqrt = (max_colors * max_colors) as f64 / max_bins as f64 >= 0.022;
    for i in 0..max_bins {
        if i + 1 < max_bins {
            bins[i].fw = i + 1;
            bins[i + 1].bk = i;
        }
        if use_sqrt {
            bins[i].count = bins[i].count.sqrt();
        }
    }

    let deleted = (KEYS - 1) as u32;
    let mut heap = vec![0_usize; max_bins + 2];
    let mut heap_len = 0_usize;
    for i in 0..max_bins {
        find_nn(&mut bins, i);
        let err = bins[i].err;
        heap_len += 1;
        let mut l = heap_len;
        while l > 1 {
            let parent = l >> 1;
            if bins[heap[parent]].err <= err {
                break;
            }
            heap[l] = heap[parent];
            l = parent;
        }
        heap[l] = i;
    }

    let merges = max_bins.saturating_sub(max_colors);
    let mut i = 0;
    while i < merges {
        let best = loop {
            let mut b1 = heap[1];
            let tb = bins[b1];
            if tb.tm >= tb.mtm && bins[tb.nn].mtm <= tb.tm {
                break b1;
            }
            if tb.mtm == deleted {
                b1 = heap[heap_len];
                heap[1] = b1;
                heap_len -= 1;
            } else {
                find_nn(&mut bins, b1);
                bins[b1].tm = i as u32;
            }
            let err = bins[b1].err;
            let mut l = 1;
            loop {
                let mut child = l + l;
                if child > heap_len {
                    break;
                }
                if child < heap_len && bins[heap[child]].err > bins[heap[child + 1]].err {
                    child += 1;
                }
                if err <= bins[heap[child]].err {
                    break;
                }
                heap[l] = heap[child];
                l = child;
            }
            heap[l] = b1;
        };

        let nn = bins[best].nn;
        let (n1, n2) = (bins[best].count, bins[nn].count);
        let d = 1.0 / (n1 + n2);
        bins[best].r = d * (n1 * bins[best].r + n2 * bins[nn].r);
        bins[best].g = d * (n1 * bins[best].g + n2 * bins[nn].g);
        bins[best].b = d * (n1 * bins[best].b + n2 * bins[nn].b);
        bins[best].count = n1 + n2;
        i += 1;
        bins[best].mtm = i as u32;

        let (bk, fw) = (bins[nn].bk, bins[nn].fw);
        bins[bk].fw = fw;
        bins[fw].bk = bk;
        bins[nn].mtm = deleted;
    }

    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut i = 0;
    loop {
        let color = [bins[i].r, bins[i].g, bins[i].b].map(|v| v.round().clamp(0.0, 255.0) as u8);
        if !palette.contains(&color) {
            palette.push(color);
        }
        i = bins[i].fw;
        if i == 0 {
            break;
        }
    }
    palette
}

fn nearest(palette: &[[u8; 3]], (r, g, b): (f64, f64, f64)) -> u8 {
    let mut best = 0;
    let mut best_dist = f64::MAX;
    for (i, c) in palette.iter().enumerate() {
        let dist = (f64::from(c[0]) - r).powi(2) + (f64::from(c[1]) - g).powi(2) + (f64::from(c[2]) - b).powi(2);
        if dist < best_dist {
            best_dist = dist;
            best = i;
        }
    }
    best as u8
}

/// Browsers treat a shorter delay as 100 ms.
const MIN_DELAY_CS: i64 = 2;

/// Hundredths of a second each frame shows, from the stamps of the frames: rounding the difference of
/// rounded stamps carries the error, so the GIF lasts as long as the video. gif.js gave every frame
/// 80 ms and a 15 s clip at 30 fps played in 12 s.
fn delays_cs(seconds: &[f64], fps: u32) -> Vec<u16> {
    let cs = |t: f64| (t * 100.0).round() as i64;
    let frame_cs = (100.0 / f64::from(fps)).round() as i64;
    (0..seconds.len())
        .map(|i| {
            let next = seconds.get(i + 1).map_or(cs(seconds[i]) + frame_cs, |t| cs(*t));
            (next - cs(seconds[i])).clamp(MIN_DELAY_CS, i64::from(u16::MAX)) as u16
        })
        .collect()
}

/// The output size: never wider than asked, never upscaled.
fn scaled_size(width: u32, height: u32, max_width: u32) -> (u32, u32) {
    let scale = (f64::from(max_width) / f64::from(width.max(1))).min(1.0);
    let w = (f64::from(width) * scale).round().max(1.0) as u32;
    let h = (f64::from(height) * scale).round().max(1.0) as u32;
    (w, h)
}

/// What the decoder should do with the frame it has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) enum Wanted {
    Frame,
    Skip,
    Stop,
}

/// Collects sampled frames, writes the file at the end.
pub(crate) struct GifBuilder {
    fps: u32,
    max_width: u32,
    source: (u32, u32),
    size: (u32, u32),
    resizer: Resizer,
    scaled: Vec<u8>,
    histogram: Histogram,
    frames: Vec<Vec<u16>>,
    seconds: Vec<f64>,
    /// How many multiples of the frame interval have been passed.
    next_sample: u64,
}

impl GifBuilder {
    pub fn new(fps: u32, max_width: u32) -> Self {
        Self {
            fps,
            max_width,
            source: (0, 0),
            size: (0, 0),
            resizer: Resizer::new(),
            scaled: Vec::new(),
            histogram: Histogram::new(),
            frames: Vec::new(),
            seconds: Vec::new(),
            next_sample: 0,
        }
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Whether the frame at this time is sampled. A frame is kept when it reaches the next multiple of
    /// the frame interval, which holds the average rate also when the source rate is no multiple of
    /// it: the minimum gap of gif.js turned 30 fps into 10.
    pub fn wants(&mut self, seconds: f64) -> Wanted {
        if self.frames.len() >= MAX_FRAMES {
            return Wanted::Stop;
        }
        let interval = 1.0 / f64::from(self.fps);
        if seconds + GAP_TOLERANCE_SEC < self.next_sample as f64 * interval {
            return Wanted::Skip;
        }
        self.next_sample = ((seconds + GAP_TOLERANCE_SEC) / interval).floor() as u64 + 1;
        self.seconds.push(seconds);
        Wanted::Frame
    }

    /// Takes the frame `wants` asked for: tightly packed, top-down BGRX.
    pub fn take(&mut self, width: u32, height: u32, bgrx: &[u8]) {
        if self.source != (width, height) {
            self.source = (width, height);
            if self.frames.is_empty() {
                self.size = scaled_size(width, height, self.max_width);
                self.scaled = vec![0; self.size.0 as usize * self.size.1 as usize * 4];
            }
        }
        let pixels = if self.size == self.source {
            bgrx
        } else {
            self.scale(bgrx);
            &self.scaled
        };
        let mut keys = Vec::with_capacity(pixels.len() / 4);
        let h = &mut self.histogram;
        for px in pixels.chunks_exact(4) {
            let (b, g, r) = (px[0], px[1], px[2]);
            let key = key_of(r, g, b);
            h.r[key] += u64::from(r);
            h.g[key] += u64::from(g);
            h.b[key] += u64::from(b);
            h.count[key] += 1;
            keys.push(key as u16);
        }
        self.frames.push(keys);
    }

    fn scale(&mut self, bgrx: &[u8]) {
        let source = ImageRef::new(self.source.0, self.source.1, bgrx, PixelType::U8x4)
            .expect("the frame has the size the decoder reported");
        let mut target = Image::from_slice_u8(self.size.0, self.size.1, &mut self.scaled, PixelType::U8x4)
            .expect("the buffer was allocated for this size");
        // the fourth channel is padding, not alpha
        let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)).use_alpha(false);
        self.resizer.resize(&source, &mut target, &options).expect("sizes and pixel types match");
    }

    #[cfg(test)]
    fn frame_count(&self) -> usize {
        self.frames.len()
    }

    #[cfg(test)]
    fn offer(&mut self, seconds: f64, width: u32, height: u32, bgrx: &[u8]) -> bool {
        match self.wants(seconds) {
            Wanted::Frame => {
                self.take(width, height, bgrx);
                true
            }
            Wanted::Skip => true,
            Wanted::Stop => false,
        }
    }

    /// Writes next to the destination first: an interrupted export leaves no broken GIF.
    pub fn finish(self, out: &Path) -> Result<()> {
        let palette = quantize(&self.histogram, 256);
        if self.frames.is_empty() || palette.is_empty() {
            return Err(Error::msg("no frames decoded"));
        }
        let mut lookup = vec![0_u8; KEYS];
        for (key, slot) in lookup.iter_mut().enumerate() {
            if let Some(mean) = self.histogram.mean(key) {
                *slot = nearest(&palette, mean);
            }
        }
        let flat: Vec<u8> = palette.iter().flatten().copied().collect();
        let partial = out.with_extension("gif.part");
        let write = || -> std::result::Result<(), gif::EncodingError> {
            let file = File::create(&partial)?;
            let (w, h) = (self.size.0 as u16, self.size.1 as u16);
            let mut encoder = gif::Encoder::new(BufWriter::new(file), w, h, &flat)?;
            encoder.set_repeat(gif::Repeat::Infinite)?;
            let mut indices = vec![0_u8; self.size.0 as usize * self.size.1 as usize];
            let delays = delays_cs(&self.seconds, self.fps);
            for (keys, delay) in self.frames.iter().zip(delays) {
                for (index, &key) in indices.iter_mut().zip(keys) {
                    *index = lookup[usize::from(key)];
                }
                let frame = gif::Frame {
                    width: w,
                    height: h,
                    delay,
                    buffer: Cow::Borrowed(&indices),
                    ..gif::Frame::default()
                };
                encoder.write_frame(&frame)?;
            }
            // Dropping a BufWriter swallows the error of its last write: a full disk would publish an empty GIF.
            encoder.into_inner()?.flush()?;
            Ok(())
        };
        let written = write();
        if let Err(e) = written {
            let _ = fs::remove_file(&partial);
            return Err(Error::msg(format!("Cannot write {}: {e}", out.display())));
        }
        fs::rename(&partial, out).map_err(|e| Error::io("write", out, e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_bgrx(width: u32, height: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
        std::iter::repeat_n([b, g, r, 0], (width * height) as usize).flatten().collect()
    }

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("shotik-gif-{}-{name}", std::process::id()))
    }

    fn histogram_of(colors: &[[u8; 3]]) -> Histogram {
        let mut h = Histogram::new();
        for c in colors {
            let key = key_of(c[0], c[1], c[2]);
            h.r[key] += u64::from(c[0]);
            h.g[key] += u64::from(c[1]);
            h.b[key] += u64::from(c[2]);
            h.count[key] += 1;
        }
        h
    }

    #[test]
    fn keys_pack_like_gifenc() {
        assert_eq!(key_of(0, 0, 0), 0);
        assert_eq!(key_of(255, 255, 255), 0xf800 | 0x03e0 | 0x1f);
        assert_eq!(key_of(8, 0, 0), 0x0800);
        assert_eq!(key_of(0, 8, 0), 0x0020);
        assert_eq!(key_of(0, 0, 8), 0x0001);
    }

    #[test]
    fn few_colours_come_through_exactly() {
        let colors = [[10, 20, 30], [200, 100, 50], [0, 255, 0]];
        let palette = quantize(&histogram_of(&colors), 256);
        assert_eq!(palette.len(), 3);
        for c in colors {
            assert!(palette.contains(&c), "{c:?} in {palette:?}");
        }
    }

    #[test]
    fn many_colours_reduce_to_the_limit_and_stay_close() {
        let mut colors = Vec::new();
        for r in (0..=255).step_by(8) {
            for g in (0..=255).step_by(8) {
                colors.push([r as u8, g as u8, 128]);
            }
        }
        let palette = quantize(&histogram_of(&colors), 256);
        assert!(palette.len() <= 256 && palette.len() > 200, "{}", palette.len());
        let worst = colors
            .iter()
            .map(|c| {
                let target = (f64::from(c[0]), f64::from(c[1]), f64::from(c[2]));
                let p = palette[usize::from(nearest(&palette, target))];
                (f64::from(p[0]) - target.0).abs().max((f64::from(p[1]) - target.1).abs())
            })
            .fold(0.0, f64::max);
        assert!(worst < 16.0, "worst channel error {worst}");
    }

    #[test]
    fn delays_add_up_to_the_length_of_the_video() {
        let seconds: Vec<f64> = (0..120).map(|i| f64::from(i) / 12.0).collect();
        let delays = delays_cs(&seconds, 12);
        assert_eq!(delays.iter().map(|&d| u32::from(d)).sum::<u32>(), 1000);
        assert!(delays.iter().all(|d| (8..=9).contains(d)), "{delays:?}");
    }

    #[test]
    fn delays_follow_an_uneven_source() {
        assert_eq!(delays_cs(&[0.0, 0.5, 0.6], 12), vec![50, 10, 8]);
        assert_eq!(delays_cs(&[0.0], 25), vec![4]);
        assert_eq!(delays_cs(&[0.0, 0.001, 0.2], 12), vec![2, 20, 8], "never below what browsers honour");
    }

    #[test]
    fn size_keeps_the_aspect_and_never_upscales() {
        assert_eq!(scaled_size(1280, 720, 640), (640, 360));
        assert_eq!(scaled_size(320, 180, 640), (320, 180));
        assert_eq!(scaled_size(641, 361, 640), (640, 360));
        assert_eq!(scaled_size(4000, 2, 640), (640, 1));
    }

    #[test]
    fn frames_are_sampled_at_the_rate_like_gif_js() {
        let mut gif = GifBuilder::new(12, 640);
        let frame = solid_bgrx(4, 2, 1, 2, 3);
        for i in 0..90 {
            assert!(gif.offer(f64::from(i) / 30.0, 4, 2, &frame));
        }
        assert_eq!(gif.frame_count(), 36, "3 s at 12 fps");
        let mut slow = GifBuilder::new(12, 640);
        for i in 0..30 {
            assert!(slow.offer(f64::from(i) * 0.1, 4, 2, &frame));
        }
        assert_eq!(slow.frame_count(), 30, "a slower source keeps every frame");
    }

    #[test]
    fn sampling_stops_at_the_frame_limit() {
        let mut gif = GifBuilder::new(12, 640);
        let frame = solid_bgrx(2, 2, 1, 2, 3);
        let mut offered = 0;
        while gif.offer(f64::from(offered), 2, 2, &frame) {
            offered += 1;
            assert!(offered < 1000);
        }
        assert_eq!(gif.frame_count(), MAX_FRAMES);
    }

    #[test]
    fn the_file_decodes_to_the_frames_with_one_global_palette_and_loops() {
        let out = temp("roundtrip.gif");
        let mut gif = GifBuilder::new(12, 4);
        for i in 0_u8..3 {
            gif.offer(f64::from(i), 8, 4, &solid_bgrx(8, 4, 40 + i * 100, 10, 200 - i * 50));
        }
        gif.finish(&out).unwrap();

        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let mut decoder = options.read_info(File::open(&out).unwrap()).unwrap();
        assert_eq!((decoder.width(), decoder.height()), (4, 2));
        assert!(decoder.global_palette().is_some());
        let mut count = 0_u8;
        while let Some(frame) = decoder.read_next_frame().unwrap() {
            assert!(frame.palette.is_none(), "no local palette");
            let (r, g, b) = (frame.buffer[0], frame.buffer[1], frame.buffer[2]);
            assert_eq!((r, g, b), (40 + count * 100, 10, 200 - count * 50));
            count += 1;
        }
        assert_eq!(count, 3);
        assert_eq!(decoder.repeat(), gif::Repeat::Infinite);
        fs::remove_file(&out).unwrap();
    }

    #[test]
    fn nothing_to_write_is_an_error_and_leaves_no_file() {
        let out = temp("empty.gif");
        let gif = GifBuilder::new(12, 640);
        assert!(gif.finish(&out).is_err());
        assert!(!out.exists());
    }
}
