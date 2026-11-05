//! Native save and folder dialogs. Implements `Dialogs`.
//!
//! The port takes no parent window, so a dialog runs on its own thread with its own apartment and modal
//! loop. Callers block until the user answers, on a blocking thread as the ports document says.

use std::path::{Path, PathBuf};

use shotik_core::error::Result;
use shotik_core::ports::Dialogs;
use windows::core::{w, Interface, HRESULT, PCWSTR};
use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FileOpenDialog, FileSaveDialog, IFileDialog, IFileOpenDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName,
    FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};

use crate::displays::{wide, win_err};
use crate::shell::on_com_thread;

/// Points the dialog at a folder. A folder that no longer exists is not worth failing the dialog: it
/// opens where the shell last was, which is what the JS version gets from Electron too.
fn set_folder(dialog: &IFileDialog, folder: &Path) -> Result<()> {
    let name = wide(folder);
    // SAFETY: the name is NUL-terminated and COM is initialized by `on_com_thread`.
    let item: windows::core::Result<IShellItem> =
        unsafe { SHCreateItemFromParsingName(PCWSTR(name.as_ptr()), None) };
    if let Ok(item) = item {
        // SAFETY: both objects are alive.
        unsafe { dialog.SetFolder(&item) }.map_err(|e| win_err("cannot set the dialog's folder", e))?;
    }
    Ok(())
}

fn add_options(dialog: &IFileDialog, extra: windows::Win32::UI::Shell::FILEOPENDIALOGOPTIONS) -> Result<()> {
    // SAFETY: plain getter and setter on a live dialog.
    unsafe {
        let options = dialog.GetOptions().map_err(|e| win_err("cannot read the dialog's options", e))?;
        dialog.SetOptions(options | extra).map_err(|e| win_err("cannot set the dialog's options", e))
    }
}

/// `dialog.showSaveDialog({ defaultPath, filters: [{ name: 'PNG', extensions: ['png'] }] })`.
fn configure_save(dialog: &IFileSaveDialog, default_path: &Path) -> Result<()> {
    let filters = [COMDLG_FILTERSPEC { pszName: w!("PNG"), pszSpec: w!("*.png") }];
    // SAFETY: the filter array and its strings outlive the call, which copies them.
    unsafe { dialog.SetFileTypes(&filters) }.map_err(|e| win_err("cannot set the dialog's filter", e))?;
    // SAFETY: one-based, and the filter above is the first.
    unsafe { dialog.SetFileTypeIndex(1) }.map_err(|e| win_err("cannot select the dialog's filter", e))?;
    // SAFETY: a literal string on a live dialog.
    unsafe { dialog.SetDefaultExtension(w!("png")) }.map_err(|e| win_err("cannot set the default extension", e))?;
    let base: IFileDialog = dialog.cast().map_err(|e| win_err("IFileDialog", e))?;
    add_options(&base, FOS_OVERWRITEPROMPT | FOS_PATHMUSTEXIST | FOS_FORCEFILESYSTEM)?;
    if let Some(name) = default_path.file_name() {
        let name = wide(name);
        // SAFETY: NUL-terminated, copied by the call.
        unsafe { dialog.SetFileName(PCWSTR(name.as_ptr())) }.map_err(|e| win_err("cannot set the file name", e))?;
    }
    if let Some(folder) = default_path.parent() {
        set_folder(&base, folder)?;
    }
    Ok(())
}

/// `dialog.showOpenDialog({ properties: ['openDirectory'], defaultPath })`.
fn configure_pick_directory(dialog: &IFileOpenDialog, default_path: &Path) -> Result<()> {
    let base: IFileDialog = dialog.cast().map_err(|e| win_err("IFileDialog", e))?;
    add_options(&base, FOS_PICKFOLDERS | FOS_PATHMUSTEXIST | FOS_FORCEFILESYSTEM)?;
    set_folder(&base, default_path)
}

/// Shows the dialog and returns the chosen path, or None when the user cancelled.
fn show(dialog: &IFileDialog) -> Result<Option<PathBuf>> {
    // SAFETY: a live dialog with no owner window; blocks in its own modal loop.
    if let Err(e) = unsafe { dialog.Show(None) } {
        if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) {
            return Ok(None);
        }
        return Err(win_err("the file dialog failed", e));
    }
    // SAFETY: Show succeeded, so a result exists.
    let item = unsafe { dialog.GetResult() }.map_err(|e| win_err("cannot read the dialog's result", e))?;
    // SAFETY: the returned string is freed below with the allocator the API documents.
    let path = unsafe {
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(|e| win_err("cannot read the chosen path", e))?;
        let path = name.to_string();
        CoTaskMemFree(Some(name.0 as *const _));
        path
    };
    let path = path.map_err(|e| shotik_core::Error::msg(format!("the chosen path is not valid text: {e}")))?;
    Ok(Some(PathBuf::from(path)))
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WinDialogs;

impl WinDialogs {
    pub fn new() -> Self {
        Self
    }
}

impl Dialogs for WinDialogs {
    fn save_png(&self, default_path: &Path) -> Result<Option<PathBuf>> {
        let default_path = default_path.to_owned();
        on_com_thread("shotik-save-dialog", move || {
            // SAFETY: COM is initialized on this thread by `on_com_thread`.
            let dialog: IFileSaveDialog = unsafe { CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| win_err("cannot create the save dialog", e))?;
            configure_save(&dialog, &default_path)?;
            show(&dialog.cast().map_err(|e| win_err("IFileDialog", e))?)
        })
    }

    fn pick_directory(&self, default_path: &Path) -> Result<Option<PathBuf>> {
        let default_path = default_path.to_owned();
        on_com_thread("shotik-folder-dialog", move || {
            // SAFETY: COM is initialized on this thread by `on_com_thread`.
            let dialog: IFileOpenDialog = unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| win_err("cannot create the folder dialog", e))?;
            configure_pick_directory(&dialog, &default_path)?;
            show(&dialog.cast().map_err(|e| win_err("IFileDialog", e))?)
        })
    }
}

#[cfg(test)]
mod tests {
    //! Everything but `Show`: a dialog object is created and configured, then dropped without appearing.
    use super::*;

    fn file_name(dialog: &IFileDialog) -> String {
        // SAFETY: the returned string is freed with the documented allocator.
        unsafe {
            let name = dialog.GetFileName().unwrap();
            let text = name.to_string().unwrap();
            CoTaskMemFree(Some(name.0 as *const _));
            text
        }
    }

    #[test]
    fn the_save_dialog_is_set_up_like_the_javascript_one() {
        on_com_thread("t", || {
            // SAFETY: COM is initialized on this thread.
            let dialog: IFileSaveDialog = unsafe { CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| win_err("create", e))?;
            let default = std::env::temp_dir().join("Shot.png");
            configure_save(&dialog, &default)?;

            let base: IFileDialog = dialog.cast().unwrap();
            assert_eq!(file_name(&base), "Shot.png");
            // SAFETY: plain getters on a live dialog.
            unsafe {
                assert_eq!(base.GetFileTypeIndex().unwrap(), 1, "the first and only filter");
                let options = base.GetOptions().unwrap();
                assert_eq!(options & FOS_OVERWRITEPROMPT, FOS_OVERWRITEPROMPT);
                assert_eq!(options & FOS_FORCEFILESYSTEM, FOS_FORCEFILESYSTEM);
                assert!(base.GetFolder().is_ok(), "the folder was set");
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn the_folder_picker_picks_folders_and_starts_at_the_default() {
        on_com_thread("t", || {
            // SAFETY: COM is initialized on this thread.
            let dialog: IFileOpenDialog = unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| win_err("create", e))?;
            configure_pick_directory(&dialog, &std::env::temp_dir())?;
            let base: IFileDialog = dialog.cast().unwrap();
            // SAFETY: plain getters on a live dialog.
            unsafe {
                let options = base.GetOptions().unwrap();
                assert_eq!(options & FOS_PICKFOLDERS, FOS_PICKFOLDERS);
                assert!(base.GetFolder().is_ok());
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn a_missing_default_folder_does_not_stop_the_dialog() {
        on_com_thread("t", || {
            // SAFETY: COM is initialized on this thread.
            let dialog: IFileSaveDialog = unsafe { CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| win_err("create", e))?;
            configure_save(&dialog, Path::new(r"C:\definitely\not\here\Shot.png"))?;
            assert_eq!(file_name(&dialog.cast().unwrap()), "Shot.png");
            Ok(())
        })
        .unwrap();
    }
}
