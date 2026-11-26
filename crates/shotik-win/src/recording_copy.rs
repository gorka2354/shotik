use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Seek};
use std::mem::{offset_of, size_of, size_of_val};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, Prefix};

use shotik_core::ports::{RecordingCopy, RecordingPart};
use shotik_core::recording_copy::{part_path, Content};
use windows::Win32::Foundation::{
    ERROR_BAD_NETPATH, ERROR_BAD_NET_NAME, ERROR_CONNECTION_UNAVAIL, ERROR_HANDLE_DISK_FULL, ERROR_INVALID_DRIVE,
    ERROR_INVALID_PASSWORD, ERROR_LOCK_VIOLATION, ERROR_LOGON_FAILURE, ERROR_NETNAME_DELETED,
    ERROR_NETWORK_ACCESS_DENIED, ERROR_NETWORK_UNREACHABLE, ERROR_NOT_CONNECTED, ERROR_NO_NETWORK,
    ERROR_PATH_NOT_FOUND, ERROR_SHARING_VIOLATION, GENERIC_READ, GENERIC_WRITE, HANDLE, WIN32_ERROR,
};
use windows::Win32::Storage::FileSystem::{
    FileDispositionInfo, FileRenameInfo, SetFileInformationByHandle, DELETE, FILE_DISPOSITION_INFO,
    FILE_FLAG_DELETE_ON_CLOSE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_RENAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ,
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
            | ERROR_NOT_CONNECTED
            | ERROR_PATH_NOT_FOUND
            | ERROR_INVALID_DRIVE,
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
            .access_mode(GENERIC_WRITE.0 | DELETE.0)
            .share_mode(FILE_SHARE_READ.0)
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

    fn fingerprint(&self, path: &Path) -> io::Result<Content> {
        open_recording(path).and_then(Content::read).map_err(share_error)
    }

    fn publish(&self, source: &Path, destination: &Path) -> io::Result<()> {
        let content = self.fingerprint(source)?;
        let part = self.stage(source, &part_path(destination), content)?;
        let result = part.publish(destination);
        if result.is_err() {
            part.discard()?;
        }
        result
    }

    fn stage(&self, source: &Path, path: &Path, content: Content) -> io::Result<Box<dyn RecordingPart + '_>> {
        let mut input = open_recording(source).map_err(share_error)?;
        // Check the same handle that will be copied, before any replacement bytes reach the share.
        Content::read(&mut input).map_err(share_error)?.verify(content)?;
        input.rewind().map_err(share_error)?;
        let mut file = create_part(path).map_err(share_error)?;
        let result = Content::copy(input, &mut file)
            .and_then(|actual| actual.verify(content))
            .and_then(|()| file.sync_all());
        let part = WindowsPart(file);
        if let Err(e) = result {
            part.discard()?;
            return Err(share_error(e));
        }
        Ok(Box::new(part))
    }

    fn reopen_part(&self, path: &Path, content: Content) -> io::Result<Box<dyn RecordingPart + '_>> {
        let mut file = OpenOptions::new()
            .read(true)
            .access_mode(GENERIC_READ.0 | DELETE.0)
            .share_mode(FILE_SHARE_READ.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(path)
            .map_err(share_error)?;
        if !file.metadata().map_err(share_error)?.is_file() {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "staged recording is not a regular file",
            ));
        }
        Content::read(&mut file).map_err(share_error)?.verify(content)?;
        Ok(Box::new(WindowsPart(file)))
    }
}

/// Rename the verified *handle*. Resolving its former .part path again would let a concurrent
/// replacement publish different bytes. The handle also prevents renames by other writers.
struct WindowsPart(File);

impl RecordingPart for WindowsPart {
    fn publish(&self, target: &Path) -> io::Result<()> {
        let name = rename_path(target)?;
        let offset = offset_of!(FILE_RENAME_INFO, FileName);
        let byte_len = size_of_val(name.as_slice());
        let total = u32::try_from(offset + byte_len).map_err(|_| ErrorKind::InvalidInput)?;
        // The trailing FileName[1] is variable length. Allocate aligned storage, with
        // ReplaceIfExists false and RootDirectory null, for an absolute destination.
        let mut info = vec![FILE_RENAME_INFO::default(); (total as usize).div_ceil(size_of::<FILE_RENAME_INFO>())];
        info[0].FileNameLength = (byte_len - size_of::<u16>()) as u32;
        // SAFETY: the full UTF-16 name including NUL fits in the allocation after the fixed header.
        // All bytes used by FileRenameInfo stay live for the call and the file handle has DELETE access.
        unsafe {
            let data = info.as_mut_ptr().cast::<u8>();
            std::ptr::copy_nonoverlapping(name.as_ptr(), data.add(offset).cast::<u16>(), name.len());
            SetFileInformationByHandle(HANDLE(self.0.as_raw_handle()), FileRenameInfo, data.cast(), total)
        }
        .map_err(win_io)
    }

    fn discard(&self) -> io::Result<()> {
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: the handle has DELETE access and the buffer is a complete disposition structure.
        unsafe {
            SetFileInformationByHandle(
                HANDLE(self.0.as_raw_handle()),
                FileDispositionInfo,
                (&raw const disposition).cast(),
                size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        }
        .map_err(win_io)
    }
}

fn win_io(error: windows::core::Error) -> io::Error {
    share_error(io::Error::from_raw_os_error(error.code().0 & 0xffff))
}

fn open_recording(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_DELETE).0)
        .open(path)
}

/// Get a Win32 extended-length name lexically. Resolving ancestor handles on SMB may require
/// read access where the user only has traversal rights.
fn rename_path(path: &Path) -> io::Result<Vec<u16>> {
    let absolute = std::path::absolute(path)?;
    let encoded = wide(&absolute);
    let Some(std::path::Component::Prefix(prefix)) = absolute.components().next() else {
        return Err(ErrorKind::InvalidInput.into());
    };
    Ok(match prefix.kind() {
        Prefix::Disk(_) => r"\\?\".encode_utf16().chain(encoded).collect(),
        Prefix::UNC(_, _) => r"\\?\UNC\".encode_utf16().chain(encoded.into_iter().skip(2)).collect(),
        _ => encoded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use shotik_core::recording_copy::{deliver, Delivery};

    #[test]
    fn a_live_part_is_protected_and_an_abandoned_part_is_replaced_before_publication() {
        // The Win32 rename name can be prepared without reaching this synthetic UNC share.
        assert_eq!(
            rename_path(Path::new(r"\\server\share\restricted\calls\Rec.m4a")).unwrap(),
            wide(r"\\?\UNC\server\share\restricted\calls\Rec.m4a")
        );
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
        assert!(fs::rename(&part, calls.join("substituted.part")).is_err());
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
