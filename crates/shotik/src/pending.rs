//! Stand-ins until the recorder crate lands. They fail out loud: nothing here pretends to record.

use shotik_core::ports::{
    AudioDevice, AudioDevices, Callback, RecStatus, RecordOptions, RecordTarget, RecorderControl, RecorderEvent,
};
use shotik_core::{Error, Result};

pub struct NoAudioDevices;

impl AudioDevices for NoAudioDevices {
    fn microphones(&self) -> Result<Vec<AudioDevice>> {
        Ok(Vec::new())
    }
}

pub struct NoRecorder;

impl RecorderControl for NoRecorder {
    fn start(&self, _target: RecordTarget, _options: RecordOptions) -> Result<Option<RecStatus>> {
        Err(Error::msg("recording is not built into this version yet"))
    }
    fn stop(&self) {}
    fn pause(&self) {}
    fn resume(&self) {}
    fn cancel(&self) {}
    fn is_recording(&self) -> bool {
        false
    }
    fn status(&self) -> RecStatus {
        RecStatus::default()
    }
    fn set_listener(&self, _events: Callback<RecorderEvent>) {}
}
