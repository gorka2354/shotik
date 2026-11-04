//! Monitors in physical pixels and global DIP. Implements `Displays`.
//!
//! Every query asks the system again: there is nothing to invalidate when a monitor comes or goes,
//! and enumeration is cheap enough (see `one_snapshot_is_cheap`). The layout itself is the pure
//! function in `dip_layout`.

use std::marker::PhantomData;
use std::sync::OnceLock;
use std::time::Duration;

use shotik_core::error::{Error, Result};
use shotik_core::geom::{Display, DisplayId, Point, PointF, Rect};
use shotik_core::ports::Displays;
use windows::core::{BOOL, PCWSTR};
use windows::Win32::Foundation::{LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, DISPLAY_DEVICEW, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONULL,
};
use windows::Win32::UI::HiDpi::{
    AreDpiAwarenessContextsEqual, GetDpiAwarenessContextForProcess, GetDpiForMonitor, SetProcessDpiAwarenessContext,
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, EDD_GET_DEVICE_INTERFACE_NAME, MONITORINFOF_PRIMARY};

use crate::dip_layout::{self, DipRects, MonitorInfo};

/// An OS failure with what we were doing when it happened.
pub(crate) fn win_err(what: &str, e: windows::core::Error) -> Error {
    Error::msg(format!("{what}: {e}"))
}

/// NUL-terminated UTF-16 for a Win32 call.
pub(crate) fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// The text before the first NUL of a fixed-size UTF-16 buffer.
pub(crate) fn from_wide_buf(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Per-monitor v2 for the whole process: true physical pixels from every API, no bitmap scaling.
/// The binary calls this first thing, before any window exists. Fine to call again.
pub fn set_dpi_awareness() -> Result<()> {
    // SAFETY: takes a predefined constant and touches only process state.
    let set = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if set.is_ok() {
        return Ok(());
    }
    // It fails when the awareness was already set, by a manifest or an earlier call. That is fine as
    // long as it is the mode we need.
    // SAFETY: both calls take process-independent constants or a null handle meaning "this process".
    let same = unsafe {
        let current = GetDpiAwarenessContextForProcess(windows::Win32::Foundation::HANDLE::default());
        AreDpiAwarenessContextsEqual(current, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
    };
    if same.as_bool() {
        return Ok(());
    }
    Err(win_err("cannot make the process per-monitor DPI aware", set.unwrap_err()))
}

/// Physical coordinates on this thread until dropped, whatever the process is set to. A port must not
/// return wrong numbers because a caller forgot `set_dpi_awareness`.
pub(crate) struct PerMonitorDpiScope {
    previous: DPI_AWARENESS_CONTEXT,
    /// The awareness belongs to a thread, so the guard must stay on it.
    _not_send: PhantomData<*const ()>,
}

impl PerMonitorDpiScope {
    pub(crate) fn enter() -> Self {
        // SAFETY: takes a predefined constant and changes only this thread's state, restored in drop.
        let previous = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        Self { previous, _not_send: PhantomData }
    }
}

impl Drop for PerMonitorDpiScope {
    fn drop(&mut self) {
        // SAFETY: `previous` came from the same call on the same thread (the guard is not Send).
        unsafe { SetThreadDpiAwarenessContext(self.previous) };
    }
}

pub(crate) fn rect_from(r: RECT) -> Rect {
    Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top)
}

/// One monitor as the system reports it right now.
#[derive(Clone, Copy)]
struct Entry {
    info: MonitorInfo,
    id: DisplayId,
    primary: bool,
}

struct Snapshot {
    entries: Vec<Entry>,
    rects: Vec<DipRects>,
}

/// The accessibility "Make text bigger" factor, which Chromium multiplies into the device scale factor
/// of every monitor (`GetScaleFactorForDPI`). 1.0 when the setting cannot be read, as in Chromium.
fn text_scale() -> f32 {
    use windows::UI::ViewManagement::UISettings;
    static SETTINGS: OnceLock<Option<UISettings>> = OnceLock::new();
    SETTINGS
        .get_or_init(|| UISettings::new().ok())
        .as_ref()
        .and_then(|s| s.TextScaleFactor().ok())
        .map_or(1.0, |f| f as f32)
}

/// Stable across restarts while the monitor stays connected. Chromium's id is not: it hashes the adapter
/// LUID, which changes on reboot. JSON readers in JavaScript lose precision above 2^53, so it stays below.
fn id_from_device_path(path: &str) -> DisplayId {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.to_ascii_lowercase().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash & ((1 << 53) - 1)
}

fn monitor_id(info: &MONITORINFOEXW) -> DisplayId {
    let mut device = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
    // SAFETY: `szDevice` is NUL-terminated by the system, `device` is writable and has `cb` set.
    let found = unsafe {
        EnumDisplayDevicesW(PCWSTR(info.szDevice.as_ptr()), 0, &mut device, EDD_GET_DEVICE_INTERFACE_NAME)
    };
    let path = if found.as_bool() { from_wide_buf(&device.DeviceID) } else { String::new() };
    // No interface path, as under some remote sessions: the GDI device name is Chromium's backup too.
    id_from_device_path(if path.is_empty() { from_wide_buf(&info.szDevice) } else { path }.as_str())
}

fn entry_for(monitor: HMONITOR, text_scale: f32) -> Option<Entry> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `info` is a MONITORINFOEXW with `cbSize` saying so, which the API accepts as MONITORINFO.
    let ok = unsafe { GetMonitorInfoW(monitor, std::ptr::from_mut(&mut info).cast::<MONITORINFO>()) };
    if !ok.as_bool() {
        // The monitor went away between enumeration and this call.
        return None;
    }
    let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
    // SAFETY: plain out-parameters on a handle the system just gave us.
    unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.ok()?;
    Some(Entry {
        info: MonitorInfo {
            physical: rect_from(info.monitorInfo.rcMonitor),
            work: rect_from(info.monitorInfo.rcWork),
            scale: dpi_x as f32 / 96.0 * text_scale,
        },
        id: monitor_id(&info),
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
    })
}

fn snapshot() -> Snapshot {
    let _dpi = PerMonitorDpiScope::enter();

    unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, out: LPARAM) -> BOOL {
        // SAFETY: `out` is the Vec below, alive and unaliased for the duration of EnumDisplayMonitors.
        unsafe { (*(out.0 as *mut Vec<HMONITOR>)).push(monitor) };
        true.into()
    }

    let scale = text_scale();
    // No monitor at the origin means we raced a display change: Chromium waits for the next notification,
    // which a pull API does not have, so it looks again a moment later.
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut monitors: Vec<HMONITOR> = Vec::new();
        // SAFETY: the callback only pushes to `monitors`, which outlives the call.
        let _ = unsafe {
            EnumDisplayMonitors(None, None, Some(collect), LPARAM(std::ptr::from_mut(&mut monitors) as isize))
        };
        let entries: Vec<Entry> = monitors.into_iter().filter_map(|m| entry_for(m, scale)).collect();
        let infos: Vec<MonitorInfo> = entries.iter().map(|e| e.info).collect();
        let rects = dip_layout::layout(&infos);
        if !rects.is_empty() {
            return Snapshot { entries, rects };
        }
    }
    Snapshot { entries: Vec::new(), rects: Vec::new() }
}

impl Snapshot {
    fn display(&self, i: usize) -> Display {
        let e = &self.entries[i];
        Display {
            id: e.id,
            bounds: self.rects[i].bounds,
            work_area: self.rects[i].work_area,
            scale_factor: f64::from(e.info.scale),
            physical_bounds: e.info.physical,
            primary: e.primary,
        }
    }

    fn physicals(&self) -> impl Iterator<Item = Rect> + '_ {
        self.entries.iter().map(|e| e.info.physical)
    }

    fn infos(&self) -> Vec<MonitorInfo> {
        self.entries.iter().map(|e| e.info).collect()
    }

    fn primary_index(&self) -> Option<usize> {
        self.entries.iter().position(|e| e.primary).or_else(|| (!self.entries.is_empty()).then_some(0))
    }
}

/// What a signature without a `Result` returns when no monitor is attached at all, as during a mode
/// change on a headless or remote session. An empty rectangle, so a capture fails with a clear error
/// instead of pointing at a monitor that does not exist.
fn no_display() -> Display {
    Display {
        id: 0,
        bounds: Rect::default(),
        work_area: Rect::default(),
        scale_factor: 1.0,
        physical_bounds: Rect::default(),
        primary: true,
    }
}

/// The handle of the monitor a `Display` describes, checked against the current configuration: a display
/// list held across a mode change must fail clearly, not capture the wrong rectangle.
pub(crate) fn monitor_handle(display: &Display) -> Result<HMONITOR> {
    let _dpi = PerMonitorDpiScope::enter();
    let p = display.physical_bounds;
    if p.width <= 0 || p.height <= 0 {
        return Err(Error::msg("no display is attached right now"));
    }
    let center = POINT { x: p.x + p.width / 2, y: p.y + p.height / 2 };
    // SAFETY: takes the point by value and returns a handle or null.
    let monitor = unsafe { MonitorFromPoint(center, MONITOR_DEFAULTTONULL) };
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: a null handle makes the call fail, `info` is writable and has `cbSize` set.
    let found = !monitor.is_invalid() && unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool();
    if !found || rect_from(info.rcMonitor) != p {
        return Err(Error::msg(format!(
            "display {} at {}x{} ({}, {}) is gone: the monitor configuration changed",
            display.id, p.width, p.height, p.x, p.y
        )));
    }
    Ok(monitor)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WinDisplays;

impl WinDisplays {
    pub fn new() -> Self {
        Self
    }
}

impl Displays for WinDisplays {
    fn all(&self) -> Vec<Display> {
        let s = snapshot();
        (0..s.entries.len()).map(|i| s.display(i)).collect()
    }

    fn primary(&self) -> Display {
        let s = snapshot();
        s.primary_index().map_or_else(no_display, |i| s.display(i))
    }

    /// Floored, like `screen.getCursorScreenPoint()`.
    fn cursor(&self) -> PointF {
        let _dpi = PerMonitorDpiScope::enter();
        let mut phys = POINT::default();
        // It fails while the secure desktop has the input. The origin is on the primary, the display
        // a UAC prompt or lock screen would use anyway.
        // SAFETY: a plain out-parameter.
        let _ = unsafe { GetCursorPos(&mut phys) };
        let dip = self.phys_to_dip(Point { x: phys.x, y: phys.y });
        PointF { x: dip.x.floor(), y: dip.y.floor() }
    }

    fn nearest(&self, point: PointF) -> Display {
        let s = snapshot();
        dip_layout::nearest_dip(&s.infos(), &s.rects, point).map_or_else(no_display, |i| s.display(i))
    }

    fn phys_to_dip(&self, point: Point) -> PointF {
        let s = snapshot();
        match dip_layout::nearest_physical(s.physicals(), point) {
            Some(i) => dip_layout::phys_to_dip(
                PointF { x: f64::from(point.x), y: f64::from(point.y) },
                s.entries[i].info.physical,
                s.entries[i].info.scale,
                s.rects[i].bounds,
            ),
            None => PointF { x: f64::from(point.x), y: f64::from(point.y) },
        }
    }

    fn phys_rect_to_dip(&self, rect: Rect) -> Rect {
        let s = snapshot();
        match dip_layout::nearest_physical_rect(s.physicals(), rect) {
            Some(i) => {
                dip_layout::phys_rect_to_dip(rect, s.entries[i].info.physical, s.entries[i].info.scale, s.rects[i].bounds)
            }
            None => rect,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST;

    #[test]
    fn id_is_deterministic_case_insensitive_and_safe_for_javascript() {
        let path = r"\\?\DISPLAY#GSM5B7F#5&2c37a1b&0&UID4352#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}";
        let id = id_from_device_path(path);
        assert_eq!(id, id_from_device_path(path));
        assert_eq!(id, id_from_device_path(&path.to_lowercase()));
        assert_ne!(id, id_from_device_path(r"\\?\DISPLAY#GSM5B7F#5&2c37a1b&0&UID4353"));
        assert!(id < 1 << 53);
        // A pinned value: this number ends up in settings.json as `lastRegion.displayId`.
        assert_eq!(id_from_device_path("abc"), 0xe71fa2190541574b_u64 & ((1 << 53) - 1));
    }

    #[test]
    fn the_real_monitors_are_consistent() {
        let d = WinDisplays::new();
        let all = d.all();
        assert!(!all.is_empty(), "this machine has monitors");
        assert_eq!(all.iter().filter(|m| m.primary).count(), 1);
        let primary = d.primary();
        assert!(primary.primary);
        assert_eq!((primary.bounds.x, primary.bounds.y), (0, 0));
        assert_eq!((primary.physical_bounds.x, primary.physical_bounds.y), (0, 0));
        for m in &all {
            // The size is what a captured frame will have, so it must not be a virtualized one.
            let enclosing = (f64::from(m.physical_bounds.width) / m.scale_factor).ceil() as i32;
            assert_eq!(m.bounds.width, enclosing, "{m:?}");
            let w = m.work_area;
            assert!(w.x >= m.bounds.x && w.y >= m.bounds.y, "{m:?}");
            assert!(w.x + w.width <= m.bounds.x + m.bounds.width, "{m:?}");
            assert!(w.y + w.height <= m.bounds.y + m.bounds.height, "{m:?}");
        }
        let mut ids: Vec<u64> = all.iter().map(|m| m.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "ids are unique");
        assert_eq!(d.all(), all, "a second look returns the same");
    }

    #[test]
    fn by_id_finds_each_monitor_and_an_unknown_id_means_the_primary() {
        let d = WinDisplays::new();
        for m in d.all() {
            assert_eq!(d.by_id(m.id), m);
        }
        assert_eq!(d.by_id(1), d.primary());
    }

    #[test]
    fn nearest_physical_agrees_with_the_system() {
        let _dpi = PerMonitorDpiScope::enter();
        let d = WinDisplays::new();
        let all = d.all();
        let mut points = vec![
            Point { x: 22000, y: 300 },
            Point { x: 22000, y: 5000 },
            Point { x: -22000, y: 300 },
            Point { x: 0, y: -100 },
            Point { x: 0, y: 30000 },
        ];
        for m in &all {
            let p = m.physical_bounds;
            points.push(Point { x: p.x, y: p.y });
            points.push(Point { x: p.x + p.width - 1, y: p.y + p.height - 1 });
            points.push(Point { x: p.x + p.width, y: p.y + p.height });
            points.push(Point { x: p.x - 1, y: p.y - 1 });
            points.push(Point { x: p.x + p.width / 2, y: p.y + p.height / 2 });
        }
        for point in points {
            let system = {
                // SAFETY: takes the point by value and returns a handle.
                let monitor = unsafe { MonitorFromPoint(POINT { x: point.x, y: point.y }, MONITOR_DEFAULTTONEAREST) };
                let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
                // SAFETY: `info` is writable and has `cbSize` set.
                assert!(unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool());
                rect_from(info.rcMonitor)
            };
            let ours = dip_layout::nearest_physical(all.iter().map(|m| m.physical_bounds), point).unwrap();
            assert_eq!(all[ours].physical_bounds, system, "at {point:?}");
        }
    }

    #[test]
    fn cursor_is_inside_a_display_and_whole() {
        let d = WinDisplays::new();
        let c = d.cursor();
        assert_eq!(c.x.fract(), 0.0);
        assert_eq!(c.y.fract(), 0.0);
        let under = d.under_cursor();
        assert!(d.all().contains(&under));
    }

    #[test]
    fn monitor_handle_checks_the_current_configuration() {
        let d = WinDisplays::new();
        for m in d.all() {
            assert!(monitor_handle(&m).is_ok(), "{m:?}");
        }
        let mut gone = d.primary();
        gone.physical_bounds.width += 1;
        let err = monitor_handle(&gone).unwrap_err().to_string();
        assert!(err.contains("is gone"), "{err}");
        assert!(monitor_handle(&no_display()).is_err());
    }

    /// A thread set to DPI-unaware, the worst a caller could do: without our scope every number would be
    /// virtualized to 96 DPI, and frames would come out the wrong size.
    pub(crate) fn on_unaware_thread<T: Send>(f: impl FnOnce() -> T + Send) -> T {
        use windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_UNAWARE;
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    // SAFETY: a predefined constant, on this short-lived thread only.
                    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_UNAWARE) };
                    f()
                })
                .join()
                .unwrap()
        })
    }

    #[test]
    fn an_unaware_thread_gets_the_same_physical_numbers() {
        let aware = WinDisplays::new().all();
        let unaware = on_unaware_thread(|| WinDisplays::new().all());
        assert_eq!(unaware, aware);
    }

    #[test]
    fn set_dpi_awareness_can_be_called_twice() {
        set_dpi_awareness().unwrap();
        set_dpi_awareness().unwrap();
    }

    #[test]
    fn one_snapshot_is_cheap() {
        let t = std::time::Instant::now();
        for _ in 0..20 {
            std::hint::black_box(snapshot());
        }
        let each = t.elapsed() / 20;
        // The overlay asks more than once per hotkey press.
        assert!(each < Duration::from_millis(20), "{each:?} per snapshot");
        println!("one snapshot: {each:?}");
    }
}
