//! The synthetic sources used when the application runs in test mode: an animated picture and two
//! tones, never a real screen or device. They run on the shared clock like the real ones, so they
//! go through the same mixing and pacing. Reference: `synthStream` and `synthAudio` in
//! `src/recorder/recorder.js`.

use std::f64::consts::TAU;

use crate::clock::TICKS_PER_SEC;
use crate::mix::{CHANNELS, SAMPLE_RATE};

/// The gain of `synthAudio`. `test/ghost/` wants more than 0.035 from an enabled input.
pub const TONE_AMPLITUDE: f64 = 0.15;
pub const SYSTEM_TONE_HZ: f64 = 440.0;
pub const MICROPHONE_TONE_HZ: f64 = 880.0;

/// The canvas of `synthStream` is the whole display. The bench region is on a 4K display.
const CANVAS_WIDTH: i64 = 3840;

/// Marks instants on the shared clock in both picture and sound, to measure their offset in a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Marker {
    pub period_ms: u32,
    pub width_ms: u32,
}

impl Marker {
    fn on(&self, t: i64) -> bool {
        let unit = TICKS_PER_SEC / 1000;
        t.rem_euclid(i64::from(self.period_ms) * unit) < i64::from(self.width_ms) * unit
    }
}

/// Stretches of nothing at all on the system input, as WASAPI loopback gives while nothing plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SilentStretches {
    pub period_ms: u32,
    pub audible_ms: u32,
}

impl SilentStretches {
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn audible(&self, t: i64) -> bool {
        let unit = TICKS_PER_SEC / 1000;
        t.rem_euclid(i64::from(self.period_ms) * unit) < i64::from(self.audible_ms) * unit
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SynthOptions {
    /// None: the animation and steady tones of the JS version. Some: black and silence, white and
    /// tone during the mark.
    pub marker: Option<Marker>,
    pub system_silence: Option<SilentStretches>,
}

/// One tone on the shared clock.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Tone {
    pub hz: f64,
    pub marker: Option<Marker>,
}

impl Tone {
    /// Interleaved stereo for `frames` frames. `first_frame` counts audio frames on the shared clock,
    /// so consecutive packets continue each other's phase exactly.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn render(&self, first_frame: i64, frames: usize, out: &mut Vec<f32>) {
        out.clear();
        out.reserve(frames * CHANNELS);
        for i in 0..frames as i128 {
            let index = i128::from(first_frame) + i;
            let at = (index * i128::from(TICKS_PER_SEC) / i128::from(SAMPLE_RATE)) as i64;
            let sounding = self.marker.is_none_or(|m| m.on(at));
            let value = if sounding {
                // the index stays small enough for f64 and keeps the phase continuous across packets
                let cycles = self.hz * (index % (i128::from(SAMPLE_RATE) * 3600)) as f64 / SAMPLE_RATE as f64;
                (TONE_AMPLITUDE * (TAU * cycles).sin()) as f32
            } else {
                0.0
            };
            out.extend_from_slice(&[value, value]);
        }
    }
}

const FONT_W: usize = 5;
const FONT_H: usize = 7;

fn glyph(c: u8) -> [u8; FONT_H] {
    match c {
        b'0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        b'1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        b'2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        b'3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        b'4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        b'5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        b'6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        b'7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        b'8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        b'9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        b'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        b'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        b'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        b'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        b'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        b'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        b'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        b'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        b'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        b'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        b'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        b'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        _ => [0; FONT_H],
    }
}

const BACKGROUND: [u8; 4] = [0x1c, 0x14, 0x12, 0xff];
const RECTANGLE: [u8; 4] = [0xff, 0x5c, 0x7c, 0xff];
const TEXT: [u8; 4] = [0xff, 0xff, 0xff, 0xff];

/// Draws the frames of the animated picture for one region of the virtual canvas.
#[derive(Clone, Debug)]
pub(crate) struct Scene {
    crop_x: i64,
    crop_y: i64,
    width: usize,
    height: usize,
    marker: Option<Marker>,
    frame: u64,
}

impl Scene {
    pub fn new(crop_x: u32, crop_y: u32, width: u32, height: u32, marker: Option<Marker>) -> Self {
        Self {
            crop_x: i64::from(crop_x),
            crop_y: i64::from(crop_y),
            width: width as usize,
            height: height as usize,
            marker,
            frame: 0,
        }
    }

    /// Paints the next frame, top-down BGRA with a stride of `width * 4`, at time `t`.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn draw(&mut self, t: i64, out: &mut [u8]) {
        self.frame += 1;
        if let Some(marker) = self.marker {
            out.fill(if marker.on(t) { 0xff } else { 0x00 });
            return;
        }
        let stride = self.width * 4;
        let (first, rest) = out.split_at_mut(stride);
        for px in first.chunks_exact_mut(4) {
            px.copy_from_slice(&BACKGROUND);
        }
        for row in rest.chunks_exact_mut(stride) {
            row.copy_from_slice(first);
        }
        // as in synthStream: 9 px per frame, 220 px wide, at y 240
        let x = (self.frame as i64 * 9).rem_euclid((CANVAS_WIDTH - 260).max(1));
        self.fill_rect(out, x, 240, 220, 220, RECTANGLE);
        self.text(out, &format!("REC SYNTHETIC FRAME {}", self.frame), 120, 100, 6);
    }

    fn fill_rect(&self, out: &mut [u8], x: i64, y: i64, w: i64, h: i64, color: [u8; 4]) {
        let left = (x - self.crop_x).max(0);
        let top = (y - self.crop_y).max(0);
        let right = (x + w - self.crop_x).min(self.width as i64);
        let bottom = (y + h - self.crop_y).min(self.height as i64);
        if left >= right {
            return;
        }
        let stride = self.width * 4;
        for row in top..bottom {
            let start = row as usize * stride;
            for px in out[start + left as usize * 4..start + right as usize * 4].chunks_exact_mut(4) {
                px.copy_from_slice(&color);
            }
        }
    }

    fn text(&self, out: &mut [u8], text: &str, x: i64, y: i64, scale: i64) {
        for (i, c) in text.bytes().enumerate() {
            let cell = x + i as i64 * (FONT_W as i64 + 1) * scale;
            for (row, bits) in glyph(c).iter().enumerate() {
                for col in 0..FONT_W {
                    if bits & (0x10 >> col) != 0 {
                        self.fill_rect(out, cell + col as i64 * scale, y + row as i64 * scale, scale, scale, TEXT);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amplitude(samples: &[f32], hz: f64) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        let left: Vec<f64> = samples.iter().step_by(CHANNELS).map(|&s| f64::from(s)).collect();
        for (i, s) in left.iter().enumerate() {
            let angle = TAU * hz * i as f64 / SAMPLE_RATE as f64;
            re += s * angle.cos();
            im += s * angle.sin();
        }
        2.0 * f64::hypot(re, im) / left.len() as f64
    }

    #[test]
    fn tones_have_the_amplitude_the_ghost_tests_measure() {
        let mut out = Vec::new();
        Tone { hz: SYSTEM_TONE_HZ, marker: None }.render(0, 48_000, &mut out);
        assert!((amplitude(&out, 440.0) - TONE_AMPLITUDE).abs() < 0.001);
        assert!(amplitude(&out, 880.0) < 0.001);
        Tone { hz: MICROPHONE_TONE_HZ, marker: None }.render(0, 48_000, &mut out);
        assert!((amplitude(&out, 880.0) - TONE_AMPLITUDE).abs() < 0.001);
    }

    #[test]
    fn the_phase_is_continuous_across_packets() {
        let tone = Tone { hz: 440.0, marker: None };
        let mut whole = Vec::new();
        tone.render(48_000, 960, &mut whole);
        let mut a = Vec::new();
        tone.render(48_000, 480, &mut a);
        let mut b = Vec::new();
        tone.render(48_480, 480, &mut b);
        a.extend(b);
        for (x, y) in whole.iter().zip(&a) {
            assert!((x - y).abs() < 1e-5);
        }
    }

    #[test]
    fn a_marked_tone_sounds_only_during_the_mark() {
        let marker = Marker { period_ms: 1000, width_ms: 100 };
        let tone = Tone { hz: 880.0, marker: Some(marker) };
        let mut out = Vec::new();
        tone.render(0, 48_000, &mut out);
        let loud: f32 = out[..4800 * 2].iter().fold(0.0, |p, s| p.max(s.abs()));
        let quiet: f32 = out[4801 * 2..].iter().fold(0.0, |p, s| p.max(s.abs()));
        assert!(loud > 0.1);
        assert_eq!(quiet, 0.0);
    }

    #[test]
    fn the_scene_moves_and_stays_inside_the_buffer() {
        let (w, h) = (64_u32, 48_u32);
        let mut scene = Scene::new(0, 230, w, h, None);
        let mut a = vec![0; (w * h * 4) as usize];
        scene.draw(0, &mut a);
        let mut b = a.clone();
        scene.draw(0, &mut b);
        assert_ne!(a, b, "the rectangle moved");
        assert_eq!(&a[..4], &BACKGROUND);
    }

    #[test]
    fn the_marker_scene_is_white_during_the_mark() {
        let marker = Marker { period_ms: 1000, width_ms: 100 };
        let mut scene = Scene::new(0, 0, 4, 2, Some(marker));
        let mut px = vec![1; 32];
        scene.draw(50 * TICKS_PER_SEC / 1000, &mut px);
        assert!(px.iter().all(|&v| v == 0xff));
        scene.draw(150 * TICKS_PER_SEC / 1000, &mut px);
        assert!(px.iter().all(|&v| v == 0));
    }
}
