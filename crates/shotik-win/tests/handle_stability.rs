//! Handles and private memory must stay flat across many captures: pass or fail. The installed Electron app
//! reached 139,628 handles and 5 GB in 16 days through a leak of about 1.5 handles per capture.
//!
//! One test in its own binary, so nothing else in the process moves the counters. Real pixels stay in memory.
#![cfg(windows)]

use shotik_core::ports::{Displays, ScreenCapture, WindowCaptureMethod, WindowSource};
use shotik_win::capture::WinCapture;
use shotik_win::displays::WinDisplays;
use shotik_win::window_source::WinWindowSource;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

/// A few handles of slack, for what the system's own thread pools do on the side.
const HANDLE_SLACK: i64 = 8;
const PRIVATE_SLACK_BYTES: i64 = 16 * 1024 * 1024;

fn counters() -> (i64, i64) {
    let mut handles = 0u32;
    let mut memory =
        PROCESS_MEMORY_COUNTERS_EX { cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
    // SAFETY: the pseudo-handle is always valid and the out-parameters are writable, with `cb` set.
    unsafe {
        let me = GetCurrentProcess();
        GetProcessHandleCount(me, &mut handles).unwrap();
        GetProcessMemoryInfo(
            me,
            std::ptr::from_mut(&mut memory).cast::<PROCESS_MEMORY_COUNTERS>(),
            size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
        .unwrap();
    }
    (i64::from(handles), memory.PrivateUsage as i64)
}

/// Runs `step` for a while to settle, then many more times, and fails if the process grew.
fn assert_flat(label: &str, settle: usize, times: usize, mut step: impl FnMut(usize)) {
    for i in 0..settle {
        step(i);
    }
    let (handles_before, private_before) = counters();
    for i in 0..times {
        step(i);
    }
    let (handles_after, private_after) = counters();
    println!("{label}: handles {handles_before} -> {handles_after}, private {private_before} -> {private_after} bytes");
    assert!(
        handles_after - handles_before <= HANDLE_SLACK,
        "{label}: {handles_before} -> {handles_after} handles over {times} runs"
    );
    assert!(
        private_after - private_before <= PRIVATE_SLACK_BYTES,
        "{label}: {private_before} -> {private_after} private bytes over {times} runs"
    );
}

#[test]
fn nothing_grows_across_many_captures() {
    let displays = WinDisplays::new().all();
    assert!(!displays.is_empty());
    let capture = WinCapture::new();
    assert_flat("screen capture", 12, 150, |i| {
        std::hint::black_box(capture.grab(&displays[i % displays.len()]).unwrap());
    });

    let windows = WinWindowSource::new();
    assert_flat("window list", 5, 150, |_| {
        std::hint::black_box(windows.list(None).unwrap());
    });

    // Someone else's window, read in memory only, when the desktop has one.
    if let Some(target) = windows.list(Some(std::process::id())).unwrap().into_iter().find(|w| !w.minimized) {
        assert_flat("PrintWindow", 5, 150, |_| {
            // A window may close meanwhile. That must not grow anything either.
            let _ = std::hint::black_box(windows.capture(target.id, WindowCaptureMethod::PrintWindow));
        });
    }
}
