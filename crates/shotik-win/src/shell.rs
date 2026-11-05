//! Reveal in folder, open a file, open a URL. Implements `Shell`.
//! Reference: `shell_win` in Electron (`platform_util_win.cc`) and Chromium's `ui/base/win/shell.cc`.
//!
//! Shell calls may load handlers that expect a COM apartment and may take a while, so each runs on its
//! own short-lived thread instead of changing the apartment of whichever thread the caller is on.

use std::path::Path;

use shotik_core::error::{Error, Result};
use shotik_core::ports::Shell;
use windows::core::{w, HRESULT, PCWSTR};
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    SHOpenFolderAndSelectItems, SHParseDisplayName, SHQueryUserNotificationState, ShellExecuteExW, QUNS_ACCEPTS_NOTIFICATIONS, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::displays::{wide, win_err};

/// COM initialized on this thread until dropped.
pub(crate) struct ComApartment {
    initialized: bool,
    /// An apartment belongs to a thread.
    _not_send: std::marker::PhantomData<*const ()>,
}

impl ComApartment {
    /// Single-threaded, which is what the shell's dialogs and handlers expect.
    pub(crate) fn single_threaded() -> Result<Self> {
        // SAFETY: no reserved argument, and balanced by CoUninitialize in drop on success.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        hr.ok().map_err(|e| win_err("cannot initialize COM", e))?;
        Ok(Self { initialized: true, _not_send: std::marker::PhantomData })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: balances the successful CoInitializeEx on the same thread (the guard is not Send).
            unsafe { CoUninitialize() };
        }
    }
}

/// Runs `f` on a new thread with a COM apartment and waits for it.
pub(crate) fn on_com_thread<T: Send + 'static>(
    name: &str,
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let _com = ComApartment::single_threaded()?;
            f()
        })
        .map_err(|e| Error::msg(format!("cannot start a shell thread: {e}")))?
        .join()
        .map_err(|_| Error::msg("a shell call panicked"))?
}

/// A shell item id list, freed on drop.
struct ItemIdList(*mut ITEMIDLIST);

impl ItemIdList {
    fn parse(path: &Path) -> Result<Self> {
        let name = wide(path);
        let mut list: *mut ITEMIDLIST = std::ptr::null_mut();
        // SAFETY: the name is NUL-terminated; `list` receives a block we free in drop. COM is initialized
        // by `on_com_thread`.
        unsafe { SHParseDisplayName(PCWSTR(name.as_ptr()), None, &mut list, 0, None) }
            .map_err(|e| win_err(&format!("cannot find {} in the shell", path.display()), e))?;
        Ok(Self(list))
    }
}

impl Drop for ItemIdList {
    fn drop(&mut self) {
        // SAFETY: allocated by the shell with CoTaskMemAlloc, freed once.
        unsafe { CoTaskMemFree(Some(self.0 as *const _)) };
    }
}

fn require_exists(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    Err(Error::msg(format!("{} does not exist", path.display())))
}

/// `ShellExecuteExW`. With SEE_MASK_NOASYNC it returns after the launch, so failures are reported here.
fn shell_execute(file: &str, directory: Option<&Path>, verb: Option<PCWSTR>, no_ui: bool) -> Result<()> {
    let file_w = wide(file);
    let directory_w = directory.map(wide);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: if no_ui { SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI } else { SEE_MASK_NOASYNC },
        lpVerb: verb.unwrap_or(PCWSTR::null()),
        lpFile: PCWSTR(file_w.as_ptr()),
        lpDirectory: directory_w.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` is fully initialized and the strings it points at outlive the call.
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| win_err(&format!("cannot open {file}"), e))
}

/// `base::EscapeExternalHandlerValue`, what Electron applies before handing a URL to the shell.
/// Everything except alphanumerics, the reserved characters `;/?:@&=+$,` and `!'()*-._~#[]` is
/// percent-escaped, and an existing `%XX` stays as it is.
fn escape_external(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    for (i, &c) in bytes.iter().enumerate() {
        let keep_escape = c == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit();
        let needs_escape = !(0x20..0x7F).contains(&c)
            || matches!(c, b' ' | b'"' | b'%' | b'<' | b'>' | b'\\' | b'^' | b'`' | b'{' | b'|' | b'}');
        if needs_escape && !keep_escape {
            out.push_str(&format!("%{c:02X}"));
        } else {
            out.push(c as char);
        }
    }
    out
}

/// A URL is handed to the shell as a program to run, so anything but a web address is refused here.
/// `src/main/main.js` checks the same before `shell.openExternal`.
fn check_web_url(url: &str) -> Result<()> {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Ok(());
    }
    Err(Error::msg(format!("refusing to open {url}: only http and https addresses are allowed")))
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WinShell;

impl WinShell {
    pub fn new() -> Self {
        Self
    }
}

impl Shell for WinShell {
    fn reveal_in_folder(&self, path: &Path) -> Result<()> {
        require_exists(path)?;
        let path = path.to_owned();
        on_com_thread("shotik-reveal", move || {
            let dir = path.parent().ok_or_else(|| Error::msg(format!("{} has no folder", path.display())))?;
            let folder = ItemIdList::parse(dir)?;
            let item = ItemIdList::parse(&path)?;
            let items = [item.0.cast_const()];
            // SAFETY: both id lists are alive for the call and the slice has the right length.
            let shown = unsafe { SHOpenFolderAndSelectItems(folder.0, Some(&items), 0) };
            match shown {
                Ok(()) => Ok(()),
                // "On some systems, the above call mysteriously fails with file not found even though
                // the file is there. In these cases, ShellExecute() seems to work as a fallback (although
                // it won't select the file)": the one failure Chromium answers this way.
                Err(e) if e.code() == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) => {
                    shell_execute(&dir.to_string_lossy(), None, Some(w!("open")), false)
                }
                Err(e) => Err(win_err(&format!("cannot show {} in its folder", path.display()), e)),
            }
        })
    }

    fn open_path(&self, path: &Path) -> Result<()> {
        require_exists(path)?;
        let path = path.to_owned();
        on_com_thread("shotik-open", move || {
            let file = path.to_string_lossy().into_owned();
            if path.is_dir() {
                shell_execute(&file, Some(&path), Some(w!("explore")), false)
            } else {
                shell_execute(&file, path.parent(), None, false)
            }
        })
    }

    fn run_installer(&self, installer: &Path, install_dir: &Path, show_main: bool) -> Result<()> {
        use std::os::windows::process::CommandExt;
        require_exists(installer)?;
        let folder = installer.parent().ok_or_else(|| Error::msg("the installer has no folder"))?;
        std::process::Command::new(installer)
            .raw_arg(installer_arguments(install_dir, show_main)?)
            .current_dir(folder)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(drop)
            .map_err(|e| Error::io("start the installer", installer, e))
    }

    fn notifications_welcome(&self) -> bool {
        // SAFETY: no pointers go in, and the state comes back by value.
        match unsafe { SHQueryUserNotificationState() } {
            Ok(state) => state == QUNS_ACCEPTS_NOTIFICATIONS,
            // Only a toast is at stake, and never hearing of an update costs more than one badly timed toast.
            Err(_) => true,
        }
    }

    fn open_url(&self, url: &str) -> Result<()> {
        check_web_url(url)?;
        // Quoted, so the handler cannot read parameters out of it.
        let quoted = format!("\"{}\"", escape_external(url));
        on_com_thread("shotik-open-url", move || shell_execute(&quoted, None, Some(w!("open")), true))
    }
}

/// NSIS reads `/D=` as the rest of the line: it comes last and takes no quotes, even around spaces. That is why this
/// is one raw string: `Command::arg` would quote it.
fn installer_arguments(install_dir: &Path, show_main: bool) -> Result<String> {
    let dir = install_dir.to_str().ok_or_else(|| Error::msg("the install folder has an unreadable name"))?;
    if dir.starts_with(r"\\?\") || !install_dir.is_absolute() {
        return Err(Error::msg(format!("the installer cannot use this install folder: {dir}")));
    }
    let show = if show_main { " /SHOWMAIN" } else { "" };
    Ok(format!("/S /RESTART{show} /D={dir}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn installer_arguments_end_with_an_unquoted_folder() {
        let dir = Path::new(r"C:\Users\John Smith\AppData\Local\Programs\shotik");
        assert_eq!(
            installer_arguments(dir, false).unwrap(),
            r"/S /RESTART /D=C:\Users\John Smith\AppData\Local\Programs\shotik"
        );
        assert_eq!(
            installer_arguments(dir, true).unwrap(),
            r"/S /RESTART /SHOWMAIN /D=C:\Users\John Smith\AppData\Local\Programs\shotik"
        );
    }

    #[test]
    fn installer_arguments_refuse_what_nsis_cannot_read() {
        assert!(installer_arguments(Path::new("relative\\folder"), false).is_err());
        assert!(installer_arguments(Path::new(r"\\?\C:\Programs\shotik"), false).is_err());
    }

    use super::*;

    #[test]
    fn urls_are_escaped_like_electron_does() {
        assert_eq!(escape_external("https://a.b/c d"), "https://a.b/c%20d");
        assert_eq!(escape_external("https://a.b/?q=\"x\"&y=<z>"), "https://a.b/?q=%22x%22&y=%3Cz%3E");
        assert_eq!(escape_external("https://a.b/a\\b^c`d{e|f}"), "https://a.b/a%5Cb%5Ec%60d%7Be%7Cf%7D");
        // An existing escape stays, a stray percent does not.
        assert_eq!(escape_external("https://a.b/%41%zz"), "https://a.b/%41%25zz");
        assert_eq!(escape_external("https://a.b/100%"), "https://a.b/100%25");
        // Everything reserved or safe is left alone.
        let keep = "https://a.b/;/?:@&=+$,!'()*-._~#[]";
        assert_eq!(escape_external(keep), keep);
        // Non-ASCII goes out as UTF-8 escapes.
        assert_eq!(escape_external("https://a.b/я"), "https://a.b/%D1%8F");
        assert_eq!(escape_external("https://a.b/\n"), "https://a.b/%0A");
    }

    #[test]
    fn only_web_addresses_reach_the_shell() {
        let shell = WinShell::new();
        for bad in ["calc.exe", "file:///C:/Windows/System32/calc.exe", "javascript:alert(1)", "ms-settings:", ""] {
            let err = shell.open_url(bad).unwrap_err().to_string();
            assert!(err.contains("only http and https"), "{bad}: {err}");
        }
        assert!(check_web_url("https://github.com/gorka2354/shotik").is_ok());
        assert!(check_web_url("HTTP://EXAMPLE.COM").is_ok());
    }

    #[test]
    fn a_missing_file_is_an_error_and_opens_nothing() {
        let shell = WinShell::new();
        let missing = Path::new(r"C:\definitely\not\here\shotik-test.png");
        assert!(shell.reveal_in_folder(missing).unwrap_err().to_string().contains("does not exist"));
        assert!(shell.open_path(missing).unwrap_err().to_string().contains("does not exist"));
    }

    #[test]
    fn com_threads_run_and_report_errors() {
        assert_eq!(on_com_thread("t", || Ok(7)).unwrap(), 7);
        let err = on_com_thread::<()>("t", || Err(Error::msg("boom"))).unwrap_err();
        assert_eq!(err.to_string(), "boom");
    }

    #[test]
    fn a_parsed_path_frees_cleanly_and_a_bad_one_is_an_error() {
        on_com_thread("t", || {
            let list = ItemIdList::parse(&std::env::temp_dir())?;
            assert!(!list.0.is_null());
            drop(list);
            let err = ItemIdList::parse(Path::new(r"C:\definitely\not\here")).err().expect("not found");
            assert!(err.to_string().contains("in the shell"), "{err}");
            Ok(())
        })
        .unwrap();
    }
}
