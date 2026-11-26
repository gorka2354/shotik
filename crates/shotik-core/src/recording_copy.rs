//! Pending audio recordings and the delivery decision, independent of the filesystem and clock.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ports::RecordingCopy;
use crate::{storage, Error, Result};

pub const RETRY_DELAY: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CopyQueue(Vec<PathBuf>);

impl CopyQueue {
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error::msg(format!("cannot read recording copy queue {}: {e}", path.display()))),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::io("read recording copy queue", path, e)),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        storage::write_json(path, self)
    }

    pub fn enqueue(&mut self, file: &Path) {
        if !self.0.iter().any(|p| p == file) {
            self.0.push(file.to_owned());
        }
    }

    pub fn pending(&self) -> &[PathBuf] {
        &self.0
    }

    pub fn finish(&mut self, file: &Path, result: &io::Result<Delivery>) {
        if result.is_ok() {
            self.0.retain(|p| p != file);
        }
    }

    pub fn retry_after(&self) -> Option<Duration> {
        (!self.0.is_empty()).then_some(RETRY_DELAY)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    SourceGone,
}

#[derive(PartialEq, Eq)]
enum Destination {
    Free,
    SameSize,
    Taken,
}

fn inspect(port: &dyn RecordingCopy, path: &Path, size: u64) -> io::Result<Destination> {
    match port.file_size(path) {
        Ok(other) if other == size => Ok(Destination::SameSize),
        Ok(_) => Ok(Destination::Taken),
        Err(e) if e.kind() == ErrorKind::IsADirectory => Ok(Destination::Taken),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Destination::Free),
        Err(e) => Err(e),
    }
}

/// Check every candidate's size, including numbered copies from a previous attempt.
pub fn deliver(port: &dyn RecordingCopy, source: &Path, folder: &Path) -> io::Result<Delivery> {
    let size = match port.file_size(source) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Delivery::SourceGone),
        result => result?,
    };
    let name = source.file_name().ok_or(ErrorKind::InvalidInput)?;
    let stem = source.file_stem().ok_or(ErrorKind::InvalidInput)?;
    for number in 1_u64.. {
        let destination = if number == 1 {
            folder.join(name)
        } else {
            let mut candidate = stem.to_owned();
            candidate.push(format!("-{number}"));
            if let Some(ext) = source.extension() {
                candidate.push(".");
                candidate.push(ext);
            }
            folder.join(candidate)
        };
        match inspect(port, &destination, size)? {
            Destination::SameSize => return Ok(Delivery::Sent),
            Destination::Taken => continue,
            Destination::Free => {}
        }
        match port.publish(source, &destination) {
            Ok(()) => return Ok(Delivery::Sent),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        // Another writer claimed this name. Recheck it once, even if the share still caches
        // the earlier negative lookup or the receiver already took the file.
        if inspect(port, &destination, size)? == Destination::SameSize {
            return Ok(Delivery::Sent);
        }
    }
    Err(io::Error::other("no recording filename available"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// The destination appears after our metadata lookup but before our rename.
    struct OtherWriter {
        size: u64,
        claimed: AtomicBool,
        numbered: AtomicBool,
    }

    impl RecordingCopy for OtherWriter {
        fn file_size(&self, path: &Path) -> io::Result<u64> {
            if path == Path::new("local/call.m4a") {
                Ok(12)
            } else if path == Path::new("shared/call.m4a") && self.claimed.load(Ordering::SeqCst) {
                Ok(self.size)
            } else {
                Err(ErrorKind::NotFound.into())
            }
        }

        fn publish(&self, _: &Path, destination: &Path) -> io::Result<()> {
            if destination == Path::new("shared/call.m4a") {
                self.claimed.store(true, Ordering::SeqCst);
                return Err(ErrorKind::AlreadyExists.into());
            }
            assert_eq!(destination, Path::new("shared/call-2.m4a"));
            self.numbered.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn a_concurrent_publication_is_deduplicated_or_gets_the_next_name() {
        for size in [12, 3] {
            let port = OtherWriter {
                size,
                claimed: false.into(),
                numbered: false.into(),
            };
            assert_eq!(
                deliver(&port, Path::new("local/call.m4a"), Path::new("shared")).unwrap(),
                Delivery::Sent
            );
            assert_eq!(port.numbered.load(Ordering::SeqCst), size != 12);
        }
    }

    #[test]
    fn failures_wait_five_minutes_and_survive_restart_until_sent_or_missing() {
        let dir = TestDir::new();
        let path = dir.path().join("queue.json");
        let (a, b) = (Path::new("first.m4a"), Path::new("second.m4a"));
        let mut queue = CopyQueue::load(&path).unwrap();
        assert_eq!(queue.retry_after(), None);
        queue.enqueue(a);
        queue.enqueue(a);
        queue.enqueue(b);
        queue.finish(a, &Err(ErrorKind::NetworkUnreachable.into()));
        queue.save(&path).unwrap();
        let mut reloaded = CopyQueue::load(&path).unwrap();
        assert_eq!(reloaded.pending(), &[a, b]);
        assert_eq!(reloaded.retry_after(), Some(Duration::from_secs(300)));
        reloaded.finish(b, &Ok(Delivery::SourceGone));
        assert_eq!(reloaded.pending(), &[a]);
        reloaded.finish(a, &Ok(Delivery::Sent));
        reloaded.save(&path).unwrap();
        assert_eq!(CopyQueue::load(&path).unwrap().retry_after(), None);
    }
}
