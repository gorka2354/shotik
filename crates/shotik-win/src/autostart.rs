//! Start at login. Implements `Autostart`. Reference: `app.setLoginItemSettings` in Electron, which
//! this replaces in place: same registry key and value name, so an upgrade overwrites the old entry.
//!
//! One deliberate difference: the executable path is quoted. Electron writes it bare, which runs
//! `C:\Program.exe` first when the path has a space.

use std::path::{Path, PathBuf};

use shotik_core::error::{Error, Result};
use shotik_core::ports::Autostart;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE,
    REG_OPTION_NON_VOLATILE, REG_SZ,
};

use crate::displays::wide;

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
/// Where Task Manager records "disabled". Electron clears it when the app enables itself.
const STARTUP_APPROVED_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run");
/// The app user model id, which Electron used as the value name.
const VALUE_NAME: &str = "dev.shotik.app";

fn reg_err(what: &str, status: WIN32_ERROR) -> Error {
    Error::msg(format!("{what}: {}", windows::core::Error::from(status)))
}

/// An open registry key, closed on drop.
struct Key(HKEY);

impl Key {
    fn open(path: PCWSTR) -> Result<Self> {
        let mut key = HKEY::default();
        // SAFETY: `path` is a NUL-terminated literal and `key` a valid out-parameter. The key is created
        // if needed, which on a new profile can be the case for StartupApproved.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path,
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(reg_err("cannot open the autostart registry key", status));
        }
        Ok(Self(key))
    }

    fn set_string(&self, name: &str, value: &str) -> Result<()> {
        let name = wide(name);
        let data: Vec<u8> = wide(value).iter().flat_map(|c| c.to_le_bytes()).collect();
        // SAFETY: the name is NUL-terminated and the data is a NUL-terminated wide string, as REG_SZ needs.
        let status = unsafe { RegSetValueExW(self.0, PCWSTR(name.as_ptr()), None, REG_SZ, Some(&data)) };
        if status != ERROR_SUCCESS {
            return Err(reg_err("cannot write the autostart value", status));
        }
        Ok(())
    }

    /// A value that is already gone is the outcome we wanted.
    fn delete(&self, name: &str) -> Result<()> {
        let name = wide(name);
        // SAFETY: the name is NUL-terminated and the key is open.
        let status = unsafe { RegDeleteValueW(self.0, PCWSTR(name.as_ptr())) };
        if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
            return Err(reg_err("cannot delete the autostart value", status));
        }
        Ok(())
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: opened by `Key::open` and closed only here.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

fn command_line(exe: &Path) -> String {
    format!("\"{}\" --hidden", exe.display())
}

pub struct WinAutostart {
    value_name: String,
    /// None means this executable.
    exe: Option<PathBuf>,
}

impl Default for WinAutostart {
    fn default() -> Self {
        Self::new()
    }
}

impl WinAutostart {
    pub fn new() -> Self {
        Self { value_name: VALUE_NAME.to_owned(), exe: None }
    }

    /// For tests: another value name, so the user's real entry stays untouched.
    pub fn with_value_name(value_name: impl Into<String>, exe: impl Into<PathBuf>) -> Self {
        Self { value_name: value_name.into(), exe: Some(exe.into()) }
    }

    fn exe(&self) -> Result<PathBuf> {
        match &self.exe {
            Some(exe) => Ok(exe.clone()),
            None => std::env::current_exe()
                .map_err(|e| Error::msg(format!("cannot tell the path of this executable: {e}"))),
        }
    }
}

impl Autostart for WinAutostart {
    fn set(&self, enabled: bool) -> Result<()> {
        let run = Key::open(RUN_KEY)?;
        let approved = Key::open(STARTUP_APPROVED_KEY)?;
        if enabled {
            run.set_string(&self.value_name, &command_line(&self.exe()?))?;
        } else {
            run.delete(&self.value_name)?;
        }
        // In both directions, as Electron does: enabling must not stay disabled by an old Task Manager
        // choice, and disabling leaves nothing behind.
        approved.delete(&self.value_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::{RegGetValueW, REG_BINARY, RRF_RT_REG_BINARY, RRF_RT_REG_SZ};

    /// Removes the injected test values even when an assertion fails.
    struct Cleanup(String);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = Key::open(RUN_KEY).and_then(|k| k.delete(&self.0));
            let _ = Key::open(STARTUP_APPROVED_KEY).and_then(|k| k.delete(&self.0));
        }
    }

    fn test_name(tag: &str) -> Cleanup {
        Cleanup(format!("dev.shotik.app.test-{tag}-{}", std::process::id()))
    }

    fn read_string(key: PCWSTR, name: &str) -> Option<String> {
        let name = wide(name);
        let mut buf = [0u16; 1024];
        let mut size = size_of_val(&buf) as u32;
        // SAFETY: the buffer and its size in bytes are valid for the call; the strings are NUL-terminated.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key,
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        (status == ERROR_SUCCESS).then(|| crate::displays::from_wide_buf(&buf))
    }

    fn approved_exists(name: &str) -> bool {
        let name = wide(name);
        let mut buf = [0u8; 64];
        let mut size = buf.len() as u32;
        // SAFETY: as above.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                STARTUP_APPROVED_KEY,
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_BINARY,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        status == ERROR_SUCCESS
    }

    #[test]
    fn the_command_quotes_the_path_and_passes_hidden() {
        assert_eq!(
            command_line(Path::new(r"C:\Program Files\Shotik\Shotik.exe")),
            r#""C:\Program Files\Shotik\Shotik.exe" --hidden"#
        );
    }

    #[test]
    fn the_default_name_is_the_one_electron_wrote() {
        assert_eq!(WinAutostart::new().value_name, "dev.shotik.app");
    }

    #[test]
    fn enabling_writes_the_value_and_disabling_removes_it() {
        let name = test_name("roundtrip");
        let auto = WinAutostart::with_value_name(&name.0, r"C:\Some Dir\Shotik.exe");

        auto.set(true).unwrap();
        assert_eq!(read_string(RUN_KEY, &name.0).as_deref(), Some(r#""C:\Some Dir\Shotik.exe" --hidden"#));

        auto.set(false).unwrap();
        assert_eq!(read_string(RUN_KEY, &name.0), None);

        // Disabling what is already disabled is not an error.
        auto.set(false).unwrap();
    }

    #[test]
    fn enabling_clears_a_task_manager_disable_like_electron() {
        let name = test_name("approved");
        let wname = wide(&name.0);
        // What Task Manager writes for "disabled".
        let disabled = [3u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let key = Key::open(STARTUP_APPROVED_KEY).unwrap();
        // SAFETY: the name is NUL-terminated and the data is a plain byte slice.
        let status = unsafe { RegSetValueExW(key.0, PCWSTR(wname.as_ptr()), None, REG_BINARY, Some(&disabled)) };
        assert_eq!(status, ERROR_SUCCESS);
        assert!(approved_exists(&name.0));

        WinAutostart::with_value_name(&name.0, r"C:\Shotik.exe").set(true).unwrap();
        assert!(!approved_exists(&name.0), "the enable must win");
        assert!(read_string(RUN_KEY, &name.0).is_some());
    }

    #[test]
    fn the_real_entry_is_not_touched_by_the_tests() {
        // Reads the user's real value twice around an injected change. Whatever it holds must not move.
        let before = read_string(RUN_KEY, VALUE_NAME);
        let name = test_name("isolation");
        let auto = WinAutostart::with_value_name(&name.0, r"C:\Shotik.exe");
        auto.set(true).unwrap();
        auto.set(false).unwrap();
        assert_eq!(read_string(RUN_KEY, VALUE_NAME), before);
    }
}
