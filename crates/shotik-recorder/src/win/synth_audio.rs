//! A tone delivered like a capture device: packets stamped with the shared clock, and optionally
//! nothing at all for stretches of time, the way loopback behaves while nothing plays.

use std::thread;
use std::time::Duration;

use shotik_core::Result;

use super::com::qpc_ticks;
use crate::backend::{AudioInput, AudioPacket};
use crate::mix::{frames_to_ticks, ticks_to_frames, SAMPLE_RATE};
use crate::synth::{SilentStretches, Tone};

const PACKET_PERIOD: Duration = Duration::from_millis(5);
/// 1 ms: fewer frames are left for the next read.
const MIN_PACKET_FRAMES: i64 = SAMPLE_RATE / 1000;

pub(crate) struct SynthInput {
    tone: Tone,
    silence: Option<SilentStretches>,
    next_frame: i64,
    samples: Vec<f32>,
}

impl SynthInput {
    pub fn new(tone: Tone, silence: Option<SilentStretches>) -> Self {
        Self { tone, silence, next_frame: ticks_to_frames(qpc_ticks()), samples: Vec::new() }
    }
}

impl AudioInput for SynthInput {
    fn read(&mut self, deliver: &mut dyn FnMut(AudioPacket<'_>)) -> Result<()> {
        thread::sleep(PACKET_PERIOD);
        let frames = ticks_to_frames(qpc_ticks()) - self.next_frame;
        if frames < MIN_PACKET_FRAMES {
            return Ok(());
        }
        let at = frames_to_ticks(self.next_frame);
        if self.silence.is_none_or(|s| s.audible(at)) {
            self.tone.render(self.next_frame, frames as usize, &mut self.samples);
            deliver(AudioPacket { at, samples: &self.samples });
        }
        self.next_frame += frames;
        Ok(())
    }
}
