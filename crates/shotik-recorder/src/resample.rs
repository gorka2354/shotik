//! Any device format to the session's: stereo f32 at 48 kHz, the most the AAC encoder takes. A shared
//! mode mix format can be 192 kHz with any number of channels, on loopback and on the microphone
//! independently, and it changes when the user changes a setting. Electron's recording of calls fails
//! outright on such a device.
//!
//! The resampler is a polyphase windowed sinc. Its stepping is exact rational arithmetic, so it has no
//! drift of its own, and every output packet keeps the time it was captured at on the shared clock.

use std::f64::consts::PI;

use crate::clock::TICKS_PER_SEC;
use crate::mix::{CHANNELS, SAMPLE_RATE};

/// The filter reaches this many zero crossings of the sinc to each side, at the lower of both rates.
const ZERO_CROSSINGS: usize = 32;
/// The cutoff as a part of the lower Nyquist: with this filter length flat to about 20 kHz, with the
/// stop band from 24 kHz.
const CUTOFF: f64 = 0.92;
/// Kaiser window, about 80 dB of stop band.
const KAISER_BETA: f64 = 8.0;
/// Rates with an unusual ratio share phases instead of filling memory.
const MAX_PHASES: usize = 1024;
/// A stamp this far from where the previous packet ended means a break in the stream, as loopback has
/// when nothing plays: the filter starts over.
const BREAK_TICKS: i64 = TICKS_PER_SEC / 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Encoding {
    F32,
    I16,
    I24,
    I32,
}

impl Encoding {
    fn bytes(self) -> usize {
        match self {
            Self::I16 => 2,
            Self::I24 => 3,
            Self::F32 | Self::I32 => 4,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::F32 => "float32",
            Self::I16 => "pcm16",
            Self::I24 => "pcm24",
            Self::I32 => "pcm32",
        }
    }
}

/// What a device delivers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeviceFormat {
    pub rate: u32,
    pub channels: u16,
    pub encoding: Encoding,
    /// The `SPEAKER_*` bits of the channels, zero when the format does not say.
    pub channel_mask: u32,
    /// Bytes of one frame.
    pub block_align: u16,
}

impl std::fmt::Display for DeviceFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} Hz, {} channels, {}", self.rate, self.channels, self.encoding.name())
    }
}

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

impl DeviceFormat {
    /// Reads the bytes of a `WAVEFORMATEX`, or of the `WAVEFORMATEXTENSIBLE` that begins with one.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < 16 {
            return Err(format!("a wave format of {} bytes", bytes.len()));
        }
        let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        let mut tag = u16_at(0);
        let channels = u16_at(2);
        let rate = u32_at(4);
        let block_align = u16_at(12);
        let bits = u16_at(14);
        let mut channel_mask = 0;
        if tag == WAVE_FORMAT_EXTENSIBLE {
            if bytes.len() < 40 {
                return Err(format!("an extensible wave format of {} bytes", bytes.len()));
            }
            channel_mask = u32_at(20);
            // the first field of the subformat GUID is the plain format tag
            tag = u32_at(24) as u16;
        }
        let encoding = match (tag, bits) {
            (WAVE_FORMAT_IEEE_FLOAT, 32) => Encoding::F32,
            (WAVE_FORMAT_PCM, 16) => Encoding::I16,
            (WAVE_FORMAT_PCM, 24) => Encoding::I24,
            (WAVE_FORMAT_PCM, 32) => Encoding::I32,
            _ => return Err(format!("sample format {tag} with {bits} bits is not supported")),
        };
        if channels == 0 || rate == 0 {
            return Err(format!("{channels} channels at {rate} Hz"));
        }
        if usize::from(block_align) < usize::from(channels) * encoding.bytes() {
            return Err(format!("{block_align} bytes per frame for {channels} channels of {}", encoding.name()));
        }
        Ok(Self { rate, channels, encoding, channel_mask, block_align })
    }
}

const SPEAKER_FRONT_LEFT: u32 = 0x1;
const SPEAKER_FRONT_RIGHT: u32 = 0x2;
const SPEAKER_FRONT_CENTER: u32 = 0x4;
const SPEAKER_LOW_FREQUENCY: u32 = 0x8;
const SPEAKER_BACK_LEFT: u32 = 0x10;
const SPEAKER_BACK_RIGHT: u32 = 0x20;
const SPEAKER_FRONT_LEFT_OF_CENTER: u32 = 0x40;
const SPEAKER_FRONT_RIGHT_OF_CENTER: u32 = 0x80;
const SPEAKER_BACK_CENTER: u32 = 0x100;
const SPEAKER_SIDE_LEFT: u32 = 0x200;
const SPEAKER_SIDE_RIGHT: u32 = 0x400;

/// The usual layout for a format that does not name its speakers.
fn default_mask(channels: u16) -> u32 {
    match channels {
        4 => SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT | SPEAKER_BACK_LEFT | SPEAKER_BACK_RIGHT,
        6 => SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT | SPEAKER_FRONT_CENTER | SPEAKER_LOW_FREQUENCY | SPEAKER_SIDE_LEFT | SPEAKER_SIDE_RIGHT,
        8 => {
            SPEAKER_FRONT_LEFT
                | SPEAKER_FRONT_RIGHT
                | SPEAKER_FRONT_CENTER
                | SPEAKER_LOW_FREQUENCY
                | SPEAKER_BACK_LEFT
                | SPEAKER_BACK_RIGHT
                | SPEAKER_SIDE_LEFT
                | SPEAKER_SIDE_RIGHT
        }
        _ => 0,
    }
}

/// How much of each device channel goes to left and right. The downmix of ITU-R BS.775 without the
/// LFE. It is not scaled down: full level on all channels at once is rare, and then it clips, where a
/// scale would make the ordinary case of sound on the front pair much quieter.
fn stereo_gains(format: &DeviceFormat) -> Vec<(f32, f32)> {
    const SIDE: f32 = std::f32::consts::FRAC_1_SQRT_2;
    match format.channels {
        1 => return vec![(1.0, 1.0)],
        2 => return vec![(1.0, 0.0), (0.0, 1.0)],
        _ => {}
    }
    let mask = if format.channel_mask != 0 { format.channel_mask } else { default_mask(format.channels) };
    // channels come in the order of their mask bits; any beyond the named ones alternate sides
    let mut speakers = (0..32).map(|bit| 1_u32 << bit).filter(|bit| mask & bit != 0);
    (0..format.channels)
        .map(|index| match speakers.next() {
            Some(SPEAKER_FRONT_LEFT) => (1.0, 0.0),
            Some(SPEAKER_FRONT_RIGHT) => (0.0, 1.0),
            Some(SPEAKER_FRONT_CENTER) => (SIDE, SIDE),
            Some(SPEAKER_LOW_FREQUENCY) => (0.0, 0.0),
            Some(SPEAKER_BACK_LEFT | SPEAKER_SIDE_LEFT | SPEAKER_FRONT_LEFT_OF_CENTER) => (SIDE, 0.0),
            Some(SPEAKER_BACK_RIGHT | SPEAKER_SIDE_RIGHT | SPEAKER_FRONT_RIGHT_OF_CENTER) => (0.0, SIDE),
            Some(SPEAKER_BACK_CENTER) => (0.5, 0.5),
            _ if index % 2 == 0 => (SIDE, 0.0),
            _ => (0.0, SIDE),
        })
        .collect()
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// Modified Bessel function of the first kind, order zero.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    for k in 1..64 {
        term *= (x / (2.0 * f64::from(k))).powi(2);
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

/// Stereo at one rate to stereo at another. Output frame `n` lies at input position `n * in / out`,
/// and it is produced once the input reaches past that position by the reach of the filter.
#[derive(Debug)]
pub(crate) struct Resampler {
    /// Input frames per output frame, in lowest terms.
    step_num: u64,
    step_den: u64,
    /// Filter coefficients, `taps` for each of `phases`.
    coefficients: Vec<f32>,
    taps: usize,
    phases: usize,
    /// Input frames the filter reaches to the right of its position.
    reach: i64,
    /// Interleaved input not yet fully used. Its first frame has index `base`.
    history: Vec<f32>,
    base: i64,
    /// The position of the next output frame: whole frames and a fraction of `step_den`.
    index: i64,
    fraction: u64,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        let (from64, to64) = (u64::from(from), u64::from(to));
        let divisor = gcd(from64, to64);
        let (step_num, step_den) = (from64 / divisor, to64 / divisor);
        let phases = (step_den as usize).min(MAX_PHASES);
        // the cutoff follows the lower rate; below the input rate the filter gets longer by the ratio
        let narrowing = (to64 as f64 / from64 as f64).min(1.0);
        let reach = (ZERO_CROSSINGS as f64 / narrowing).ceil() as i64;
        let taps = reach as usize * 2;
        let window_norm = bessel_i0(KAISER_BETA);
        let mut coefficients = vec![0.0_f32; taps * phases];
        for (phase, row) in coefficients.chunks_exact_mut(taps).enumerate() {
            let offset = phase as f64 / phases as f64;
            let mut values: Vec<f64> = (0..taps)
                .map(|tap| {
                    // tap positions run from -reach + 1 to reach, relative to the whole-frame position
                    let x = (tap as f64 - reach as f64 + 1.0) - offset;
                    let edge = x / reach as f64;
                    if edge.abs() >= 1.0 {
                        return 0.0;
                    }
                    let window = bessel_i0(KAISER_BETA * (1.0 - edge * edge).sqrt()) / window_norm;
                    sinc(narrowing * CUTOFF * x) * window
                })
                .collect();
            // each phase passes a constant unchanged, or the phases would ripple at the low rate
            let sum: f64 = values.iter().sum();
            values.iter_mut().for_each(|v| *v /= sum);
            for (out, value) in row.iter_mut().zip(&values) {
                *out = *value as f32;
            }
        }
        let mut resampler = Self {
            step_num,
            step_den,
            coefficients,
            taps,
            phases,
            reach,
            history: Vec::new(),
            base: 0,
            index: 0,
            fraction: 0,
        };
        resampler.reset();
        resampler
    }

    /// Forgets the past: the next input frame is index zero, with silence before it.
    pub fn reset(&mut self) {
        self.history.clear();
        // what the first outputs reach back to
        self.history.resize((self.reach as usize - 1) * CHANNELS, 0.0);
        self.base = -(self.reach - 1);
        self.index = 0;
        self.fraction = 0;
    }

    /// The input frame index the next `process` starts at.
    fn end(&self) -> i64 {
        self.base + (self.history.len() / CHANNELS) as i64
    }

    /// Takes interleaved stereo and appends what can be produced to `out`. Returns the position, in
    /// input frames relative to the first frame of this input, of the first frame appended.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> f64 {
        let first_input = self.end();
        self.history.extend_from_slice(input);
        let end = self.end();
        let position = (self.index - first_input) as f64 + self.fraction as f64 / self.step_den as f64;
        let (whole_step, fraction_step) = (self.step_num / self.step_den, self.step_num % self.step_den);
        while self.index + self.reach < end {
            let phase = (self.fraction as usize * self.phases) / self.step_den as usize;
            let start = (self.index - self.reach + 1 - self.base) as usize * CHANNELS;
            let frames = &self.history[start..start + self.taps * CHANNELS];
            let row = &self.coefficients[phase * self.taps..(phase + 1) * self.taps];
            let (mut left, mut right) = (0.0_f32, 0.0_f32);
            for (coefficient, frame) in row.iter().zip(frames.chunks_exact(CHANNELS)) {
                left += coefficient * frame[0];
                right += coefficient * frame[1];
            }
            out.extend_from_slice(&[left, right]);
            self.index += whole_step as i64;
            self.fraction += fraction_step;
            if self.fraction >= self.step_den {
                self.fraction -= self.step_den;
                self.index += 1;
            }
        }
        let keep_from = self.index - self.reach + 1;
        if keep_from > self.base {
            let drop_frames = ((keep_from - self.base) as usize).min(self.history.len() / CHANNELS);
            self.history.drain(..drop_frames * CHANNELS);
            self.base += drop_frames as i64;
        }
        position
    }
}

/// One device's packets to the session's format, with the time of each.
#[derive(Debug)]
pub(crate) struct Converter {
    format: DeviceFormat,
    gains: Vec<(f32, f32)>,
    resampler: Option<Resampler>,
    stereo: Vec<f32>,
    out: Vec<f32>,
    expected_at: Option<i64>,
}

impl Converter {
    pub fn new(format: DeviceFormat) -> Self {
        let resampler = (i64::from(format.rate) != SAMPLE_RATE).then(|| Resampler::new(format.rate, SAMPLE_RATE as u32));
        Self { format, gains: stereo_gains(&format), resampler, stereo: Vec::new(), out: Vec::new(), expected_at: None }
    }

    fn decode_sample(encoding: Encoding, bytes: &[u8]) -> f32 {
        match encoding {
            Encoding::F32 => f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            Encoding::I16 => f32::from(i16::from_le_bytes([bytes[0], bytes[1]])) / 32_768.0,
            Encoding::I24 => (i32::from_le_bytes([0, bytes[0], bytes[1], bytes[2]]) >> 8) as f32 / 8_388_608.0,
            Encoding::I32 => i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32 / 2_147_483_648.0,
        }
    }

    /// Takes one packet, `frames` frames captured from time `at`, or silence when `bytes` is None. The
    /// converted audio goes to `deliver` with the time of its first frame.
    pub fn push(&mut self, bytes: Option<&[u8]>, frames: usize, at: i64, deliver: &mut dyn FnMut(i64, &[f32])) {
        self.stereo.clear();
        self.stereo.resize(frames * CHANNELS, 0.0);
        if let Some(bytes) = bytes {
            let sample_bytes = self.format.encoding.bytes();
            for (frame, target) in bytes.chunks_exact(usize::from(self.format.block_align)).zip(self.stereo.chunks_exact_mut(CHANNELS)) {
                for (channel, (left, right)) in self.gains.iter().enumerate() {
                    if *left != 0.0 || *right != 0.0 {
                        let sample = Self::decode_sample(self.format.encoding, &frame[channel * sample_bytes..]);
                        target[0] += left * sample;
                        target[1] += right * sample;
                    }
                }
            }
        }
        let duration = i128::from(TICKS_PER_SEC) * frames as i128 / i128::from(self.format.rate);
        let Some(resampler) = &mut self.resampler else {
            deliver(at, &self.stereo);
            return;
        };
        if self.expected_at.is_some_and(|expected| (at - expected).abs() > BREAK_TICKS) {
            resampler.reset();
        }
        self.expected_at = Some(at + duration as i64);
        self.out.clear();
        let position = resampler.process(&self.stereo, &mut self.out);
        if !self.out.is_empty() {
            let offset = position * TICKS_PER_SEC as f64 / f64::from(self.format.rate);
            deliver(at + offset.round() as i64, &self.out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    fn sine(rate: u32, hz: f64, amplitude: f64, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let v = (amplitude * (TAU * hz * i as f64 / f64::from(rate)).sin()) as f32;
                [v, v]
            })
            .collect()
    }

    /// Amplitude of one frequency in the left channel of a stretch of the output.
    fn amplitude(stereo: &[f32], rate: f64, hz: f64) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        let frames = stereo.len() / 2;
        for i in 0..frames {
            let angle = TAU * hz * i as f64 / rate;
            let s = f64::from(stereo[i * 2]);
            re += s * angle.cos();
            im += s * angle.sin();
        }
        2.0 * f64::hypot(re, im) / frames as f64
    }

    /// The strongest frequency near `hz`: a pitch error shows here even when the level is fine.
    fn peak_near(stereo: &[f32], rate: f64, hz: f64) -> f64 {
        (-30..=30)
            .map(|i| hz + f64::from(i) * 0.1)
            .max_by(|a, b| amplitude(stereo, rate, *a).total_cmp(&amplitude(stereo, rate, *b)))
            .unwrap()
    }

    fn run(from: u32, to: u32, input: &[f32], packet_frames: usize) -> Vec<f32> {
        let mut resampler = Resampler::new(from, to);
        let mut out = Vec::new();
        for packet in input.chunks(packet_frames * CHANNELS) {
            resampler.process(packet, &mut out);
        }
        out
    }

    /// The case of this machine: a 192 kHz default output.
    #[test]
    fn a_440_hz_tone_at_192_khz_keeps_its_pitch_and_level() {
        let input = sine(192_000, 440.0, 0.15, 192_000);
        let out = run(192_000, 48_000, &input, 1920);
        let expected_frames = 48_000;
        assert!(out.len() / 2 <= expected_frames && out.len() / 2 > expected_frames - 64, "{} frames", out.len() / 2);
        let middle = &out[9_600..9_600 + 76_800];
        let level = amplitude(middle, 48_000.0, 440.0);
        assert!((level - 0.15).abs() < 0.0005, "level {level}");
        let pitch = peak_near(middle, 48_000.0, 440.0);
        assert!((pitch - 440.0).abs() < 0.11, "pitch {pitch}");
        assert!(amplitude(middle, 48_000.0, 880.0) < 0.0005, "no harmonics");
    }

    #[test]
    fn every_common_rate_keeps_pitch_and_level() {
        for from in [8_000_u32, 11_025, 16_000, 22_050, 32_000, 44_100, 88_200, 96_000, 176_400, 384_000] {
            let input = sine(from, 440.0, 0.25, from as usize * 3 / 4);
            let out = run(from, 48_000, &input, from as usize / 100);
            let frames = out.len() / 2;
            assert!(frames <= 36_000 && frames > 36_000 - 2048, "{from} Hz: {frames} frames");
            let middle = &out[9_600..9_600 + 48_000];
            let level = amplitude(middle, 48_000.0, 440.0);
            assert!((level - 0.25).abs() < 0.001, "{from} Hz: level {level}");
            let pitch = peak_near(middle, 48_000.0, 440.0);
            assert!((pitch - 440.0).abs() < 0.11, "{from} Hz: pitch {pitch}");
        }
    }

    #[test]
    fn what_the_output_cannot_hold_does_not_fold_back() {
        // 30 kHz would alias to 18 kHz at 48 kHz
        let input = sine(192_000, 30_000.0, 0.5, 96_000);
        let out = run(192_000, 48_000, &input, 1920);
        let middle = &out[4_800..4_800 + 24_000];
        assert!(amplitude(middle, 48_000.0, 18_000.0) < 0.0005, "alias {}", amplitude(middle, 48_000.0, 18_000.0));
        let rms = (middle.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / middle.len() as f64).sqrt();
        assert!(rms < 0.001, "rms {rms}");
    }

    #[test]
    fn the_top_of_the_audible_range_passes() {
        let input = sine(192_000, 18_000.0, 0.5, 96_000);
        let out = run(192_000, 48_000, &input, 1920);
        let middle = &out[4_800..4_800 + 24_000];
        let level = amplitude(middle, 48_000.0, 18_000.0);
        assert!((level - 0.5).abs() < 0.005, "level {level}");
    }

    #[test]
    fn packet_size_makes_no_difference() {
        let input = sine(192_000, 997.0, 0.3, 9_600);
        let whole = run(192_000, 48_000, &input, 9_600);
        for size in [7, 480, 1920] {
            let split = run(192_000, 48_000, &input, size);
            assert_eq!(split.len(), whole.len(), "packets of {size}");
            for (a, b) in split.iter().zip(&whole) {
                assert!((a - b).abs() < 1e-6, "packets of {size}");
            }
        }
    }

    #[test]
    fn a_click_stays_where_it_was() {
        for from in [192_000_u32, 44_100, 96_000] {
            let click = from as usize / 8;
            let mut input = vec![0.0_f32; from as usize / 2];
            input[click * 2] = 1.0;
            input[click * 2 + 1] = 1.0;
            let out = run(from, 48_000, &input, 1000);
            let peak = out.iter().step_by(2).enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
            assert!(peak.abs_diff(6_000) <= 1, "{from} Hz: at frame {peak}");
        }
    }

    #[test]
    fn the_stream_has_no_drift_of_its_own() {
        let mut resampler = Resampler::new(44_100, 48_000);
        let mut out = Vec::new();
        let packet = vec![0.0_f32; 441 * 2];
        let mut total = 0;
        for _ in 0..1000 {
            out.clear();
            resampler.process(&packet, &mut out);
            total += out.len() / 2;
        }
        // the stepping is rational: however long this runs, only the reach of the filter is missing
        let exact = 10 * 48_000;
        assert!(total <= exact && exact - total <= 40, "{total} frames for {exact}");
    }

    fn pcm_format(rate: u32, channels: u16, encoding: Encoding) -> DeviceFormat {
        DeviceFormat { rate, channels, encoding, channel_mask: 0, block_align: channels * encoding.bytes() as u16 }
    }

    fn convert(format: DeviceFormat, bytes: &[u8], frames: usize, at: i64) -> Vec<(i64, Vec<f32>)> {
        let mut converter = Converter::new(format);
        let mut out = Vec::new();
        converter.push(Some(bytes), frames, at, &mut |t, samples| out.push((t, samples.to_vec())));
        out
    }

    #[test]
    fn stereo_at_48_khz_passes_unchanged() {
        let bytes: Vec<u8> = [0.25_f32, -0.5].iter().flat_map(|s| s.to_le_bytes()).collect();
        let out = convert(pcm_format(48_000, 2, Encoding::F32), &bytes, 1, 1234);
        assert_eq!(out, vec![(1234, vec![0.25, -0.5])]);
    }

    #[test]
    fn integer_samples_scale_to_full_range() {
        let i16_bytes: Vec<u8> = [16_384_i16, -16_384].iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(convert(pcm_format(48_000, 2, Encoding::I16), &i16_bytes, 1, 0)[0].1, vec![0.5, -0.5]);
        let i32_bytes: Vec<u8> = [1_073_741_824_i32, -1_073_741_824].iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(convert(pcm_format(48_000, 2, Encoding::I32), &i32_bytes, 1, 0)[0].1, vec![0.5, -0.5]);
        // 0x400000 and -0x400000 in three bytes
        let i24_bytes = [0x00, 0x00, 0x40, 0x00, 0x00, 0xC0];
        assert_eq!(convert(pcm_format(48_000, 2, Encoding::I24), &i24_bytes, 1, 0)[0].1, vec![0.5, -0.5]);
    }

    #[test]
    fn a_mono_microphone_goes_to_both_sides() {
        let bytes = 0.5_f32.to_le_bytes();
        assert_eq!(convert(pcm_format(48_000, 1, Encoding::F32), &bytes, 1, 0)[0].1, vec![0.5, 0.5]);
    }

    #[test]
    fn surround_folds_down_without_the_lfe() {
        let samples = [1.0_f32, 0.0, 0.5, 1.0, 0.0, 0.25];
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let out = convert(pcm_format(48_000, 6, Encoding::F32), &bytes, 1, 0);
        let side = std::f32::consts::FRAC_1_SQRT_2;
        let (left, right) = (out[0].1[0], out[0].1[1]);
        assert!((left - (1.0 + 0.5 * side)).abs() < 1e-6, "{left}");
        assert!((right - (0.5 * side + 0.25 * side)).abs() < 1e-6, "{right}");
    }

    #[test]
    fn the_stated_speakers_win_over_the_usual_layout() {
        // four channels that are not the usual quadraphonic: front pair and a side pair
        let mut format = pcm_format(48_000, 4, Encoding::F32);
        format.channel_mask = SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT | SPEAKER_SIDE_LEFT | SPEAKER_SIDE_RIGHT;
        let gains = stereo_gains(&format);
        let side = std::f32::consts::FRAC_1_SQRT_2;
        assert_eq!(gains, vec![(1.0, 0.0), (0.0, 1.0), (side, 0.0), (0.0, side)]);
        // more channels than the mask names alternate sides
        let wide = stereo_gains(&pcm_format(48_000, 3, Encoding::F32));
        assert_eq!(wide, vec![(side, 0.0), (0.0, side), (side, 0.0)]);
    }

    #[test]
    fn silence_flagged_by_the_device_takes_its_time_on_the_timeline() {
        let mut converter = Converter::new(pcm_format(192_000, 2, Encoding::F32));
        let mut total = 0;
        converter.push(None, 1920, 0, &mut |_, samples| {
            assert!(samples.iter().all(|s| *s == 0.0));
            total += samples.len() / 2;
        });
        assert!(total > 400 && total <= 480, "{total}");
    }

    /// The output is stamped with when it was captured: the stamps of consecutive packets continue
    /// each other exactly.
    #[test]
    fn stamps_stay_on_the_shared_clock() {
        let format = pcm_format(192_000, 2, Encoding::F32);
        let mut converter = Converter::new(format);
        let block: Vec<u8> = vec![0; 1920 * 8];
        let mut stamps = Vec::new();
        let tick = TICKS_PER_SEC / 100;
        for i in 0..20_i64 {
            converter.push(Some(&block), 1920, 5_000_000 + i * tick, &mut |t, samples| stamps.push((t, samples.len() / 2)));
        }
        let mut frames_before = 0_i64;
        for (t, frames) in &stamps {
            // frame n of the output is at n / 48 kHz after the first input frame
            let expected = 5_000_000 + frames_before * TICKS_PER_SEC / 48_000;
            assert!((t - expected).abs() <= 2, "stamp {t}, expected {expected}");
            frames_before += *frames as i64;
        }
        assert!(frames_before > 9_000, "{frames_before}");
    }

    /// Loopback delivers nothing while nothing plays. After the break the output starts at the new
    /// stamp, which `InputTrack` turns into silence in between.
    #[test]
    fn a_break_in_the_stream_restarts_the_filter_at_the_new_time() {
        let format = pcm_format(192_000, 2, Encoding::F32);
        let mut converter = Converter::new(format);
        let block: Vec<u8> = vec![0; 1920 * 8];
        let mut stamps = Vec::new();
        converter.push(Some(&block), 1920, 0, &mut |t, _| stamps.push(t));
        let later = TICKS_PER_SEC * 3;
        converter.push(Some(&block), 1920, later, &mut |t, _| stamps.push(t));
        assert_eq!(stamps[0], 0);
        assert_eq!(stamps[1], later, "starts over at the stamp, no leftover of the old position");
    }

    fn waveformat(tag: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
        let block = channels * bits / 8;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
        bytes.extend_from_slice(&block.to_le_bytes());
        bytes.extend_from_slice(&bits.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes
    }

    fn extensible(subformat: u32, channels: u16, rate: u32, bits: u16, mask: u32) -> Vec<u8> {
        let mut bytes = waveformat(WAVE_FORMAT_EXTENSIBLE, channels, rate, bits);
        bytes[16..18].copy_from_slice(&22_u16.to_le_bytes());
        bytes.extend_from_slice(&bits.to_le_bytes());
        bytes.extend_from_slice(&mask.to_le_bytes());
        bytes.extend_from_slice(&subformat.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71]);
        bytes
    }

    #[test]
    fn device_formats_are_read_from_wave_format_structures() {
        let plain = DeviceFormat::parse(&waveformat(WAVE_FORMAT_PCM, 2, 44_100, 16)).unwrap();
        assert_eq!((plain.rate, plain.channels, plain.encoding, plain.block_align), (44_100, 2, Encoding::I16, 4));
        let float = DeviceFormat::parse(&extensible(3, 2, 192_000, 32, 3)).unwrap();
        assert_eq!((float.rate, float.channels, float.encoding, float.channel_mask), (192_000, 2, Encoding::F32, 3));
        let surround = DeviceFormat::parse(&extensible(1, 8, 96_000, 24, 0x63F)).unwrap();
        assert_eq!((surround.rate, surround.channels, surround.encoding, surround.channel_mask), (96_000, 8, Encoding::I24, 0x63F));
        assert_eq!(float.to_string(), "192000 Hz, 2 channels, float32");
    }

    #[test]
    fn a_format_it_cannot_read_is_an_explicit_error() {
        assert!(DeviceFormat::parse(&[1, 2, 3]).is_err());
        assert!(DeviceFormat::parse(&waveformat(0x1600, 2, 48_000, 16)).unwrap_err().contains("not supported"));
        assert!(DeviceFormat::parse(&waveformat(WAVE_FORMAT_IEEE_FLOAT, 2, 48_000, 64)).unwrap_err().contains("not supported"));
        assert!(DeviceFormat::parse(&waveformat(WAVE_FORMAT_PCM, 0, 48_000, 16)).is_err());
        let mut truncated = extensible(3, 2, 48_000, 32, 3);
        truncated.truncate(30);
        assert!(DeviceFormat::parse(&truncated).is_err());
    }
}
