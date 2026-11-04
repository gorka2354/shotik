//! The microphone list for the settings page.

use shotik_core::ports::{AudioDevice, AudioDevices};
use shotik_core::{Error, Result};

use crate::win::com::Apartment;
use crate::win::wasapi::list_microphones;

#[derive(Default)]
pub struct WinAudioDevices;

impl AudioDevices for WinAudioDevices {
    /// COM here runs in the multithreaded apartment, and a caller's thread may already be in another
    /// one, so the query gets a thread of its own.
    fn microphones(&self) -> Result<Vec<AudioDevice>> {
        std::thread::spawn(|| {
            let _apartment = Apartment::enter();
            list_microphones()
        })
        .join()
        .map_err(|_| Error::msg("listing microphones panicked"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads names only: no device is opened, whatever this machine has.
    #[test]
    fn listing_microphones_answers_with_ids_and_names() {
        for device in WinAudioDevices.microphones().unwrap() {
            assert!(!device.id.is_empty());
            assert!(!device.name.is_empty());
        }
    }
}
