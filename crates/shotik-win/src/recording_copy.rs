use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind};
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;

use shotik_core::ports::RecordingCopy;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    ERROR_BAD_NETPATH, ERROR_BAD_NET_NAME, ERROR_CONNECTION_UNAVAIL, ERROR_HANDLE_DISK_FULL, ERROR_INVALID_PASSWORD,
    ERROR_LOCK_VIOLATION, ERROR_LOGON_FAILURE, ERROR_NETNAME_DELETED, ERROR_NETWORK_ACCESS_DENIED,
    ERROR_NETWORK_UNREACHABLE, ERROR_NOT_CONNECTED, ERROR_NO_NETWORK, ERROR_SHARING_VIOLATION, WIN32_ERROR,
};
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, FILE_FLAG_DELETE_ON_CLOSE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
    MOVE_FILE_FLAGS,
};

use crate::displays::wide;

pub struct WindowsRecordingCopy;

fn share_error(error: io::Error) -> io::Error {
    // An unreachable share can be reported as a missing path. Only a missing source file
    // may leave the queue. Keep OS details for failures outside these common SMB cases.
    let kind = match error.raw_os_error().map(|code| WIN32_ERROR(code as u32)) {
        Some(
            ERROR_BAD_NETPATH
            | ERROR_NETNAME_DELETED
            | ERROR_BAD_NET_NAME
            | ERROR_CONNECTION_UNAVAIL
            | ERROR_NETWORK_UNREACHABLE
            | ERROR_NO_NETWORK
            | ERROR_NOT_CONNECTED,
        ) => ErrorKind::NetworkUnreachable,
        Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION) => ErrorKind::ResourceBusy,
        Some(ERROR_INVALID_PASSWORD | ERROR_LOGON_FAILURE | ERROR_NETWORK_ACCESS_DENIED) => ErrorKind::PermissionDenied,
        Some(ERROR_HANDLE_DISK_FULL) => ErrorKind::StorageFull,
        _ => return error,
    };
    io::Error::new(kind, error)
}

/// A concurrent writer has write access, which the exclusive open below refuses to share.
/// Only an abandoned part can be removed. Opening the reparse point leaves its target alone.
fn create_part(path: &Path) -> io::Result<File> {
    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode((FILE_SHARE_READ | FILE_SHARE_DELETE).0)
            .open(path)
        {
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            result => return result,
        }
        match OpenOptions::new()
            .write(true)
            .share_mode(0)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(path)
        {
            Ok(stale) => drop(stale),
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
}

impl RecordingCopy for WindowsRecordingCopy {
    fn file_size(&self, path: &Path) -> io::Result<u64> {
        let meta = fs::metadata(path).map_err(share_error)?;
        if !meta.is_file() {
            return Err(ErrorKind::IsADirectory.into());
        }
        Ok(meta.len())
    }

    fn publish(&self, source: &Path, destination: &Path) -> io::Result<()> {
        publish_file(source, destination).map_err(share_error)
    }
}

fn publish_file(source: &Path, destination: &Path) -> io::Result<()> {
    let mut input = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(source)?;
    // Use the same extended-length folder for the std I/O and the raw Win32 rename, including UNC.
    let folder = fs::canonicalize(destination.parent().ok_or(ErrorKind::InvalidInput)?)?;
    let destination = folder.join(destination.file_name().ok_or(ErrorKind::InvalidInput)?);
    let mut name = destination.as_os_str().to_owned();
    name.push(".part");
    let part = Path::new(&name);
    let mut output = create_part(part)?;
    let result = (|| {
        io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        let (from, to) = (wide(part), wide(&destination));
        // SAFETY: both buffers are NUL-terminated. No replace-existing or copy-across-volumes flag.
        unsafe { MoveFileExW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr()), MOVE_FILE_FLAGS(0)) }
            .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xffff))
    })();
    if result.is_err() {
        // Still holding the staging handle, so this cannot delete another writer's new part.
        // A disconnected share can leave it behind; create_part recovers it on retry.
        let _ = fs::remove_file(part);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use shotik_core::recording_copy::{deliver, Delivery};

    #[test]
    fn a_live_part_is_protected_and_an_abandoned_part_is_replaced_before_publication() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("shotik-copy-{}-{stamp}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let calls = dir.join("calls");
        fs::create_dir(&calls).unwrap();
        let source = dir.join("Rec.m4a");
        let destination = calls.join("Rec.m4a");
        let part = calls.join("Rec.m4a.part");
        fs::write(&source, b"synthetic call").unwrap();
        let active = create_part(&part).unwrap();
        assert!(deliver(&WindowsRecordingCopy, &source, &calls).is_err());
        assert!(!destination.exists());
        drop(active);
        fs::write(&part, b"interrupted").unwrap();
        assert_eq!(deliver(&WindowsRecordingCopy, &source, &calls).unwrap(), Delivery::Sent);
        assert!(!part.exists());
        assert_eq!(fs::read(&destination).unwrap(), fs::read(&source).unwrap());
        fs::write(&destination, b"other call").unwrap();
        assert_eq!(
            WindowsRecordingCopy.publish(&source, &destination).unwrap_err().kind(),
            ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&destination).unwrap(), b"other call");
        assert!(!part.exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
