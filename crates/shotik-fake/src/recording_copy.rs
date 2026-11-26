use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use shotik_core::ports::{RecordingCopy, RecordingPart};
use shotik_core::recording_copy::{part_path, Content};

/// Synthetic recording bytes and publication attempts.
#[derive(Default)]
pub struct FakeRecordingCopy {
    pub files: Mutex<HashMap<PathBuf, Vec<u8>>>,
    pub failure: Mutex<Option<ErrorKind>>,
    pub attempts: Mutex<Vec<PathBuf>>,
}

impl RecordingCopy for FakeRecordingCopy {
    fn file_size(&self, path: &Path) -> io::Result<u64> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .map(|bytes| bytes.len() as u64)
            .ok_or(ErrorKind::NotFound.into())
    }

    fn fingerprint(&self, path: &Path) -> io::Result<Content> {
        let files = self.files.lock().unwrap();
        Content::read(files.get(path).ok_or(ErrorKind::NotFound)?.as_slice())
    }

    fn source_absent(&self, path: &Path) -> io::Result<bool> {
        Ok(!self.files.lock().unwrap().contains_key(path))
    }

    fn publish(&self, source: &Path, destination: &Path) -> io::Result<()> {
        let content = self.fingerprint(source)?;
        self.stage(source, &part_path(destination), content)?
            .publish(destination)
    }

    fn stage(&self, source: &Path, path: &Path, content: Content) -> io::Result<Box<dyn RecordingPart + '_>> {
        self.attempts.lock().unwrap().push(path.with_extension(""));
        if let Some(kind) = *self.failure.lock().unwrap() {
            return Err(kind.into());
        }
        let mut files = self.files.lock().unwrap();
        let bytes = files.get(source).ok_or(ErrorKind::NotFound)?.clone();
        Content::read(bytes.as_slice())?.verify(content)?;
        files.insert(path.to_owned(), bytes);
        Ok(Box::new(FakePart {
            port: self,
            path: path.to_owned(),
            content,
        }))
    }

    fn reopen_part(&self, path: &Path, content: Content) -> io::Result<Box<dyn RecordingPart + '_>> {
        self.fingerprint(path)?.verify(content)?;
        Ok(Box::new(FakePart {
            port: self,
            path: path.to_owned(),
            content,
        }))
    }
}

struct FakePart<'a> {
    port: &'a FakeRecordingCopy,
    path: PathBuf,
    content: Content,
}

impl RecordingPart for FakePart<'_> {
    fn publish(&self, target: &Path) -> io::Result<()> {
        let mut files = self.port.files.lock().unwrap();
        Content::read(files.get(&self.path).ok_or(ErrorKind::NotFound)?.as_slice())?.verify(self.content)?;
        if files.contains_key(target) {
            return Err(ErrorKind::AlreadyExists.into());
        }
        let bytes = files.remove(&self.path).unwrap();
        files.insert(target.to_owned(), bytes);
        Ok(())
    }

    fn discard(&self) -> io::Result<()> {
        self.port.files.lock().unwrap().remove(&self.path);
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
            (source.to_owned(), vec![1; 12]),
            (dir.join("Rec_1.2.m4a"), vec![2; 8]),
            (dir.join("Rec_1.2-2.m4a"), vec![3; 4]),
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
        port.files.lock().unwrap().insert(source.to_owned(), vec![1; 9]);
        *port.failure.lock().unwrap() = Some(ErrorKind::NotFound);
        assert_eq!(deliver(&port, source, dir).unwrap_err().kind(), ErrorKind::NotFound);
        port.files.lock().unwrap().remove(source);
        assert_eq!(deliver(&port, source, dir).unwrap(), Delivery::SourceGone);
    }
}
