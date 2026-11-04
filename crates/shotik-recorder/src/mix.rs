//! Placing captured audio on the timeline and mixing the inputs. Stereo f32 at 48 kHz throughout.
//!
//! A device runs on its own clock and loopback delivers nothing while nothing plays, so each packet
//! is placed by its stamp: gaps become silence and a slow drift against the shared clock is taken
//! out frame by frame, which cannot be heard, instead of with an occasional click.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::clock::TICKS_PER_SEC;

pub const SAMPLE_RATE: i64 = 48_000;
pub const CHANNELS: usize = 2;

/// Beyond this a difference is a real hole or overlap, not drift: 20 ms.
const GAP_FRAMES: i64 = SAMPLE_RATE / 50;
/// Stamps jitter; a difference this small is left alone: 2 ms.
const DEADBAND_FRAMES: i64 = SAMPLE_RATE / 500;

pub fn ticks_to_frames(ticks: i64) -> i64 {
    ((i128::from(ticks) * i128::from(SAMPLE_RATE) + i128::from(TICKS_PER_SEC) / 2) / i128::from(TICKS_PER_SEC)) as i64
}

pub fn frames_to_ticks(frames: i64) -> i64 {
    (i128::from(frames) * i128::from(TICKS_PER_SEC) / i128::from(SAMPLE_RATE)) as i64
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrackStats {
    pub packets: u64,
    /// Silence put in place of data that never came.
    pub gap_frames: u64,
    /// Data dropped because the mix had already passed its place.
    pub late_frames: u64,
    /// Frames dropped or repeated to follow the shared clock.
    pub slipped_frames: u64,
}

/// One input's samples, waiting for the mixer.
#[derive(Debug, Default)]
pub struct InputTrack {
    buf: VecDeque<f32>,
    /// Timeline frame of `buf[0]`. None until the first packet.
    head: Option<i64>,
    pub stats: TrackStats,
}

impl InputTrack {
    fn buffered(&self) -> i64 {
        (self.buf.len() / CHANNELS) as i64
    }

    /// The timeline frame the next contiguous sample would take.
    pub fn tail(&self) -> Option<i64> {
        self.head.map(|head| head + self.buffered())
    }

    /// Takes a packet stamped for timeline frame `pos`.
    pub fn push(&mut self, pos: i64, samples: &[f32]) {
        let frames = (samples.len() / CHANNELS) as i64;
        if frames == 0 {
            return;
        }
        self.stats.packets += 1;
        let Some(tail) = self.tail() else {
            self.head = Some(pos);
            self.buf.extend(samples);
            return;
        };
        let err = pos - tail;
        if err > 0 && self.buf.is_empty() {
            // read_add yields silence up to the new head
            self.stats.gap_frames += err as u64;
            self.head = Some(pos);
            self.buf.extend(samples);
        } else if err >= GAP_FRAMES {
            self.stats.gap_frames += err as u64;
            self.buf.extend(std::iter::repeat_n(0.0, err as usize * CHANNELS));
            self.buf.extend(samples);
        } else if err <= -GAP_FRAMES {
            let skip = (-err).min(frames);
            self.stats.late_frames += skip as u64;
            self.buf.extend(&samples[skip as usize * CHANNELS..]);
        } else if err.abs() <= DEADBAND_FRAMES {
            self.buf.extend(samples);
        } else {
            self.slip(samples, frames, err);
        }
    }

    /// Follows the clock by a fraction of a percent: `err` > 0 means the device is slow.
    fn slip(&mut self, samples: &[f32], frames: i64, err: i64) {
        let count = (err.abs() - DEADBAND_FRAMES).min(frames / 100 + 1).max(1) as usize;
        let step = (frames as usize / count).max(1);
        let mut left = count;
        for (i, frame) in samples.chunks_exact(CHANNELS).enumerate() {
            let hit = left > 0 && i % step == step / 2;
            if hit {
                left -= 1;
                self.stats.slipped_frames += 1;
            }
            if hit && err < 0 {
                continue;
            }
            self.buf.extend(frame);
            if hit {
                self.buf.extend(frame);
            }
        }
    }

    /// Adds `[pos, pos + frames)` into `out`, leaving what has no data untouched.
    pub fn read_add(&mut self, pos: i64, frames: usize, out: &mut [f32]) {
        let Some(mut head) = self.head else { return };
        if head < pos {
            let stale = (pos - head).min(self.buffered());
            self.stats.late_frames += stale as u64;
            self.buf.drain(..stale as usize * CHANNELS);
            head = pos;
        }
        let lead = (head - pos) as usize;
        if lead < frames {
            let n = ((frames - lead) as i64).min(self.buffered()) as usize;
            for (i, sample) in self.buf.drain(..n * CHANNELS).enumerate() {
                out[lead * CHANNELS + i] += sample;
            }
            head += n as i64;
        }
        if self.buf.is_empty() {
            head = head.max(pos + frames as i64);
        }
        self.head = Some(head);
    }
}

/// The inputs' sum may exceed full scale: there is no gain stage, so it clips instead of wrapping.
pub fn to_pcm16(mixed: &[f32], out: &mut Vec<i16>) {
    out.clear();
    out.extend(mixed.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16));
}

/// Peak of one input since the last look, for the level meters.
#[derive(Debug, Default)]
pub struct PeakMeter(AtomicU32);

impl PeakMeter {
    pub fn update(&self, samples: &[f32]) {
        let peak = samples.iter().fold(0.0_f32, |p, s| p.max(s.abs()));
        let _ = self.0.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
            (peak > f32::from_bits(old)).then(|| peak.to_bits())
        });
    }

    /// Rounded to three decimals like `startMeters` in `src/recorder/recorder.js`.
    pub fn take(&self) -> f32 {
        let peak = f32::from_bits(self.0.swap(0, Ordering::Relaxed));
        (peak.min(1.0) * 1000.0).round() / 1000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, value: f32) -> Vec<f32> {
        vec![value; frames * CHANNELS]
    }

    fn read(track: &mut InputTrack, pos: i64, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * CHANNELS];
        track.read_add(pos, frames, &mut out);
        out
    }

    #[test]
    fn frames_and_ticks_round_trip() {
        for frames in [1, 480, 1024, 48_000, 172_800_000] {
            assert_eq!(ticks_to_frames(frames_to_ticks(frames)), frames);
        }
        assert_eq!(ticks_to_frames(TICKS_PER_SEC), SAMPLE_RATE);
    }

    #[test]
    fn contiguous_packets_come_out_as_they_went_in() {
        let mut t = InputTrack::default();
        t.push(0, &tone(480, 0.1));
        t.push(480, &tone(480, 0.2));
        let out = read(&mut t, 0, 960);
        assert!(out[..960].iter().all(|&s| s == 0.1));
        assert!(out[960..].iter().all(|&s| s == 0.2));
        assert_eq!(t.stats, TrackStats { packets: 2, ..TrackStats::default() });
    }

    #[test]
    fn a_later_first_packet_leaves_leading_silence() {
        let mut t = InputTrack::default();
        t.push(100, &tone(100, 0.5));
        let out = read(&mut t, 0, 200);
        assert!(out[..200].iter().all(|&s| s == 0.0));
        assert!(out[200..].iter().all(|&s| s == 0.5));
    }

    #[test]
    fn silence_on_loopback_does_not_stall_or_shift_the_track() {
        let mut t = InputTrack::default();
        t.push(0, &tone(480, 0.5));
        // nothing plays for a second: no packets, the mixer goes on regardless
        let a = read(&mut t, 0, 48_480);
        assert!(a[..960].iter().all(|&s| s == 0.5));
        assert!(a[960..].iter().all(|&s| s == 0.0));
        t.push(48_480, &tone(480, 0.5));
        let b = read(&mut t, 48_480, 480);
        assert!(b.iter().all(|&s| s == 0.5), "the sound after the gap is at its own time");
    }

    #[test]
    fn a_hole_while_data_is_pending_becomes_silence_at_the_right_place() {
        let mut t = InputTrack::default();
        t.push(0, &tone(480, 0.5));
        t.push(480 + GAP_FRAMES, &tone(480, 0.5));
        assert_eq!(t.stats.gap_frames, GAP_FRAMES as u64);
        let out = read(&mut t, 0, 480 + GAP_FRAMES as usize + 480);
        assert!(out[..960].iter().all(|&s| s == 0.5));
        assert!(out[960..960 + GAP_FRAMES as usize * 2].iter().all(|&s| s == 0.0));
        assert!(out[960 + GAP_FRAMES as usize * 2..].iter().all(|&s| s == 0.5));
    }

    #[test]
    fn data_for_a_time_already_mixed_is_dropped_not_shifted() {
        let mut t = InputTrack::default();
        t.push(0, &tone(4800, 0.5));
        let _ = read(&mut t, 0, 4800);
        t.push(4800 - 2400, &tone(4800, 0.7));
        assert_eq!(t.stats.late_frames, 2400);
        let out = read(&mut t, 4800, 2400);
        assert!(out.iter().all(|&s| s == 0.7));
        assert_eq!(t.tail(), Some(7200));
    }

    #[test]
    fn what_the_mix_passed_is_counted_as_late() {
        let mut t = InputTrack::default();
        t.push(0, &tone(480, 0.5));
        let out = read(&mut t, 240, 240);
        assert!(out.iter().all(|&s| s == 0.5));
        assert_eq!(t.stats.late_frames, 240);
    }

    /// A device clock off by 200 ppm for an hour: the track stays within the jitter margin and
    /// nothing is lost to holes or drops.
    fn run_drift(ppm: f64) -> (i64, TrackStats) {
        let mut t = InputTrack::default();
        let packet = 480;
        let mut true_frames = 0.0_f64;
        let mut worst = 0_i64;
        let mut cursor = 0_i64;
        let mut scratch = vec![0.0; packet * CHANNELS * 4];
        for _ in 0..360_000 {
            // the device delivers `packet` frames; on the shared clock they took a little more or less
            let stamp = true_frames.round() as i64;
            t.push(stamp, &tone(packet, 0.25));
            true_frames += packet as f64 * (1.0 + ppm / 1e6);
            worst = worst.max((t.tail().unwrap() - true_frames.round() as i64).abs());
            // the mixer keeps 100 ms behind
            let target = true_frames.round() as i64 - 4800;
            if target > cursor {
                let n = (target - cursor) as usize;
                if scratch.len() < n * CHANNELS {
                    scratch.resize(n * CHANNELS, 0.0);
                }
                scratch[..n * CHANNELS].fill(0.0);
                t.read_add(cursor, n, &mut scratch[..n * CHANNELS]);
                cursor = target;
            }
        }
        (worst, t.stats)
    }

    #[test]
    fn drift_is_followed_without_holes() {
        for ppm in [200.0, -200.0] {
            let (worst, stats) = run_drift(ppm);
            assert!(worst <= DEADBAND_FRAMES + 2, "stays within 2 ms: {worst} frames off at {ppm} ppm");
            assert_eq!(stats.gap_frames, 0, "{stats:?}");
            assert_eq!(stats.late_frames, 0, "{stats:?}");
            let expected = 480.0 * 360_000.0 * f64::abs(ppm) / 1e6;
            assert!((stats.slipped_frames as f64 - expected).abs() < 200.0, "{stats:?} vs {expected}");
        }
    }

    #[test]
    fn pcm_conversion_clips_instead_of_wrapping() {
        let mut out = Vec::new();
        to_pcm16(&[0.0, 1.0, -1.0, 2.0, -2.0, 0.5], &mut out);
        assert_eq!(out, vec![0, 32767, -32767, 32767, -32767, 16384]);
    }

    #[test]
    fn the_meter_reports_the_peak_since_the_last_look() {
        let meter = PeakMeter::default();
        meter.update(&[0.1, -0.15]);
        meter.update(&[0.05]);
        assert_eq!(meter.take(), 0.15);
        assert_eq!(meter.take(), 0.0);
        meter.update(&[1.7]);
        assert_eq!(meter.take(), 1.0);
    }
}
