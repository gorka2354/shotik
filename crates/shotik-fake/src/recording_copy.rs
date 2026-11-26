use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use shotik_core::ports::RecordingCopy;

/// Synthetic files consist only of their sizes. A failed publication leaves both files alone.
#[derive(Default)]
pub struct FakeRecordingCopy {
    pub files: Mutex<HashMap<PathBuf, u64>>,
    pub failure: Mutex<Option<ErrorKind>>,
    pub attempts: Mutex<Vec<PathBuf>>,
}

impl RecordingCopy for FakeRecordingCopy {
    fn file_size(&self, path: &Path) -> io::Result<u64> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .copied()
            .ok_or(ErrorKind::NotFound.into())
    }

    fn publish(&self, source: &Path, destination: &Path) -> io::Result<()> {
        self.attempts.lock().unwrap().push(destination.to_owned());
        if let Some(kind) = *self.failure.lock().unwrap() {
            return Err(kind.into());
        }
        let mut files = self.files.lock().unwrap();
        let size = *files.get(source).ok_or(ErrorKind::NotFound)?;
        if files.contains_key(destination) {
            return Err(ErrorKind::AlreadyExists.into());
        }
        files.insert(destination.to_owned(), size);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shotik_core::recording_copy::{deliver, Delivery};

    #[test]
    fn deliver_preserves_conflicts_and_recognizes_an_already_sent_numbered_copy() {
        let port = FakeRecordingCopy::default();
        let source = Path::new("local/Rec_1.2.m4a");
        let dir = Path::new("calls");
        port.files.lock().unwrap().extend([
            (source.to_owned(), 12),
            (dir.join("Rec_1.2.m4a"), 8),
            (dir.join("Rec_1.2-2.m4a"), 4),
        ]);
        assert_eq!(deliver(&port, source, dir).unwrap(), Delivery::Sent);
        assert_eq!(deliver(&port, source, dir).unwrap(), Delivery::Sent);
        assert_eq!(*port.attempts.lock().unwrap(), [dir.join("Rec_1.2-3.m4a")]);
        assert_eq!(port.file_size(source).unwrap(), 12);
        assert_eq!(port.file_size(&dir.join("Rec_1.2.m4a")).unwrap(), 8);
        assert_eq!(port.file_size(&dir.join("Rec_1.2-2.m4a")).unwrap(), 4);
        assert_eq!(port.file_size(&dir.join("Rec_1.2-3.m4a")).unwrap(), 12);
    }

    #[test]
    fn only_a_missing_source_leaves_the_queue_when_the_folder_is_unavailable() {
        let port = FakeRecordingCopy::default();
        let source = Path::new("local/call.m4a");
        let dir = Path::new("calls");
        port.files.lock().unwrap().insert(source.to_owned(), 9);
        *port.failure.lock().unwrap() = Some(ErrorKind::NotFound);
        assert_eq!(deliver(&port, source, dir).unwrap_err().kind(), ErrorKind::NotFound);
        port.files.lock().unwrap().remove(source);
        assert_eq!(deliver(&port, source, dir).unwrap(), Delivery::SourceGone);
    }
}
