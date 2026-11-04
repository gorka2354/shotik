//! COM, Media Foundation and thread setup as RAII values, and Windows errors as ours.

use std::marker::PhantomData;
use std::sync::OnceLock;

use shotik_core::{Error, Result};
use windows::core::w;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Media::MediaFoundation::{MFShutdown, MFStartup, MFSTARTUP_FULL, MF_VERSION};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, GetCurrentThread, SetThreadPriority,
    THREAD_PRIORITY_ABOVE_NORMAL,
};

use crate::backend::ThreadRole;

pub(crate) fn hr_error(what: &str, e: &windows::core::Error) -> Error {
    Error::msg(format!("{what}: {:#010X} {}", e.code().0 as u32, e.message()))
}

pub(crate) trait Context<T> {
    fn ctx(self, what: &str) -> Result<T>;
}

impl<T> Context<T> for windows::core::Result<T> {
    fn ctx(self, what: &str) -> Result<T> {
        self.map_err(|e| hr_error(what, &e))
    }
}

/// The shared clock: QPC in 100 ns ticks, the unit WASAPI stamps its packets with.
pub(crate) fn qpc_ticks() -> i64 {
    static FREQUENCY: OnceLock<i64> = OnceLock::new();
    let frequency = *FREQUENCY.get_or_init(|| {
        let mut f = 0;
        // SAFETY: writes to a local; cannot fail on Windows XP or later.
        let _ = unsafe { QueryPerformanceFrequency(&mut f) };
        f.max(1)
    });
    let mut counter = 0;
    // SAFETY: as above.
    let _ = unsafe { QueryPerformanceCounter(&mut counter) };
    (i128::from(counter) * 10_000_000 / i128::from(frequency)) as i64
}

/// Membership of the multithreaded apartment for one thread.
pub(crate) struct Apartment {
    owned: bool,
    /// Must be released on the thread that entered.
    _not_send: PhantomData<*const ()>,
}

impl Apartment {
    pub fn enter() -> Self {
        // SAFETY: paired with CoUninitialize in drop, on the same thread because the type is !Send.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self { owned: hr.is_ok(), _not_send: PhantomData }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: see enter.
            unsafe { CoUninitialize() };
        }
    }
}

/// Media Foundation stays started for one session and is shut down after it: nothing of it is
/// resident while the application sits in the tray.
pub(crate) struct MediaFoundation;

impl MediaFoundation {
    pub fn start() -> Result<Self> {
        // SAFETY: reference counted by the system, paired with MFShutdown in drop.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }
            .ctx("Media Foundation is not available on this system (a Windows N edition needs the Media Feature Pack)")?;
        Ok(Self)
    }
}

impl Drop for MediaFoundation {
    fn drop(&mut self) {
        // SAFETY: see start.
        let _ = unsafe { MFShutdown() };
    }
}

/// What a session thread holds for its lifetime.
pub(crate) struct ThreadSetup {
    _apartment: Apartment,
    mmcss: Option<HANDLE>,
}

impl ThreadSetup {
    pub fn enter(role: ThreadRole) -> Self {
        let apartment = Apartment::enter();
        let mut mmcss = None;
        match role {
            ThreadRole::Video => {
                // SAFETY: the pseudo handle of the current thread. A failure only costs priority.
                let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL) };
            }
            ThreadRole::AudioCapture => {
                let mut task = 0;
                // SAFETY: a static name and a local out value; the handle is reverted in drop on this
                // thread. Without it capture still runs, with less protection from load.
                mmcss = unsafe { AvSetMmThreadCharacteristicsW(w!("Audio"), &mut task) }.ok();
            }
            ThreadRole::Worker | ThreadRole::Encode | ThreadRole::AudioMix => {}
        }
        Self { _apartment: apartment, mmcss }
    }
}

impl Drop for ThreadSetup {
    fn drop(&mut self) {
        if let Some(handle) = self.mmcss {
            // SAFETY: the handle came from AvSetMmThreadCharacteristicsW on this thread.
            let _ = unsafe { AvRevertMmThreadCharacteristics(handle) };
        }
    }
}
