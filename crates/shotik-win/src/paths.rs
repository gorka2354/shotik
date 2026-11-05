//! Where things live, and what the system says about the user.

use std::path::PathBuf;

use shotik_core::{Error, Result};
use windows::Win32::Globalization::GetUserDefaultLocaleName;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Pictures, FOLDERID_RoamingAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT};

fn known_folder(id: &windows::core::GUID) -> Result<PathBuf> {
    // SAFETY: on success the returned string is valid until freed, and it is copied before the free.
    unsafe {
        let raw = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None)
            .map_err(|e| Error::msg(format!("cannot find a system folder: {e}")))?;
        let path = raw.to_string().map_err(|e| Error::msg(format!("a system folder has an invalid name: {e}")));
        CoTaskMemFree(Some(raw.as_ptr().cast()));
        Ok(PathBuf::from(path?))
    }
}

/// The folder of an installed copy: its uninstaller sits next to the executable. None for a portable one.
pub fn install_dir_of(exe: &std::path::Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    dir.join("Uninstall Shotik.exe").is_file().then(|| dir.to_path_buf())
}

/// `%APPDATA%`. The environment variable wins, so test instances can have a private profile.
pub fn app_data() -> Result<PathBuf> {
    match std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
        Some(path) => Ok(PathBuf::from(path)),
        None => known_folder(&FOLDERID_RoamingAppData),
    }
}

pub fn pictures() -> Result<PathBuf> {
    known_folder(&FOLDERID_Pictures)
}

/// A BCP-47 tag such as "ru-RU". Empty when Windows cannot say, which the callers read as English.
pub fn system_locale() -> String {
    let mut buffer = [0u16; 85];
    // SAFETY: the buffer is valid for the length passed, and the call writes at most that.
    let written = unsafe { GetUserDefaultLocaleName(&mut buffer) };
    if written <= 1 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..written as usize - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_folders_and_locale_answer() {
        assert!(pictures().unwrap().is_absolute());
        assert!(known_folder(&FOLDERID_RoamingAppData).unwrap().is_absolute());
        assert!(system_locale().contains('-'), "{}", system_locale());
    }
}
