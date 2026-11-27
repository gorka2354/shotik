//! The identity of a finished call, its durable queue and the decision to publish it.

use std::collections::HashMap;
use std::fs;
use std::io::{self, ErrorKind, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ports::{RecordingCopy, RecordingPart};
use crate::{storage, Error, Result};

pub const RETRY_DELAY: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Content {
    size: u64,
    sha256: [u8; 32],
}

impl Content {
    pub fn read(input: impl Read) -> io::Result<Self> {
        Self::copy(input, io::sink())
    }

    /// Hash the exact bytes written to the output.
    pub fn copy(mut input: impl Read, mut output: impl Write) -> io::Result<Self> {
        let mut hash = Sha256::new();
        let mut size = 0;
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = match input.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            output.write_all(&buffer[..count])?;
            hash.update(&buffer[..count]);
            size += count as u64;
        }
        Ok(Self {
            size,
            sha256: hash.finalize().into(),
        })
    }

    pub fn verify(self, expected: Self) -> io::Result<()> {
        if self != expected {
            return Err(changed_content());
        }
        Ok(())
    }
}

fn changed_content() -> io::Error {
    io::Error::new(ErrorKind::InvalidData, "recording content changed")
}

/// The result of comparing a source of the expected size to the queued digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCheck {
    modified: SystemTime,
    matches: bool,
}

impl SourceCheck {
    pub fn matched(modified: SystemTime) -> Self {
        Self {
            modified,
            matches: true,
        }
    }

    /// The caller excludes writers for the lifetime of this handle, including the subsequent copy.
    pub fn verify(input: &mut fs::File, expected: Content, cached: &mut Option<Self>) -> io::Result<()> {
        let metadata = input.metadata()?;
        if metadata.len() != expected.size {
            *cached = None;
            return Err(changed_content());
        }
        let modified = metadata.modified()?;
        let matches = match cached {
            Some(check) if check.modified == modified => check.matches,
            _ => {
                let matches = Content::read(&mut *input)? == expected;
                *cached = Some(Self { modified, matches });
                input.rewind()?;
                matches
            }
        };
        if matches {
            Ok(())
        } else {
            Err(changed_content())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Identity {
    Content(Content),
    // Distinguish pending saves until the worker can read the local file.
    Unidentified(SystemTime),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Recording {
    pub source: PathBuf,
    identity: Identity,
}

impl Recording {
    pub fn new(source: &Path, content: Option<Content>) -> Self {
        let identity = content.map_or_else(|| Identity::Unidentified(SystemTime::now()), Identity::Content);
        Self {
            source: source.to_owned(),
            identity,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Handoff {
    target: PathBuf,
    /// Normally <target>.part. A concurrent collision can change the final name after staging.
    /// None records a final file whose bytes have already been verified.
    part: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedCopy {
    #[serde(flatten)]
    pub recording: Recording,
    handoff: Option<Handoff>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_check: Option<SourceCheck>,
}

impl QueuedCopy {
    fn new(recording: Recording) -> Self {
        Self {
            recording,
            handoff: None,
            source_check: None,
        }
    }

    pub fn target(&self) -> Option<&Path> {
        self.handoff.as_ref().map(|h| h.target.as_path())
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CopyQueue(Vec<QueuedCopy>);

impl CopyQueue {
    pub fn load(path: &Path) -> Result<Self> {
        Self::load_with_log(path, |message| eprintln!("{message}"))
    }

    pub fn load_with_log(path: &Path, log: impl FnOnce(&str)) -> Result<Self> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(Error::io("read recording copy queue", path, e)),
        };
        match serde_json::from_slice(&bytes) {
            Ok(queue) => Ok(queue),
            Err(_) if serde_json::from_slice::<Vec<String>>(&bytes).is_ok() => {
                log(&format!("ignoring path-only recording copy queue {}", path.display()));
                Ok(Self::default())
            }
            Err(e) => Err(Error::msg(format!(
                "cannot read recording copy queue {}: {e}",
                path.display()
            ))),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        storage::write_json(path, self)
    }

    pub fn enqueue(&mut self, recording: Recording) {
        if !self.0.iter().any(|entry| entry.recording == recording) {
            self.0.push(QueuedCopy::new(recording));
        }
    }

    pub fn pending(&self) -> &[QueuedCopy] {
        &self.0
    }

    pub fn reserved_by_others(&self, current: &Recording) -> HashMap<PathBuf, Content> {
        let mut names = HashMap::new();
        for entry in self.0.iter().filter(|e| &e.recording != current) {
            if let (Some(handoff), Identity::Content(content)) = (&entry.handoff, &entry.recording.identity) {
                names.insert(handoff.target.clone(), *content);
                if let Some(part) = &handoff.part {
                    names.insert(part.clone(), *content);
                }
            }
        }
        names
    }

    pub fn update(&mut self, previous: &Recording, entry: &QueuedCopy) -> io::Result<()> {
        let current = self
            .0
            .iter_mut()
            .find(|e| &e.recording == previous)
            .ok_or_else(|| io::Error::other("recording is no longer queued"))?;
        *current = entry.clone();
        Ok(())
    }

    pub fn acknowledge(&mut self, receipt: &QueuedCopy) {
        self.0.retain(|e| e != receipt);
    }

    pub fn finish(&mut self, recording: &Recording) {
        self.0.retain(|e| e.recording != *recording);
    }

    pub fn retry_after(&self) -> Option<Duration> {
        (!self.0.is_empty()).then_some(RETRY_DELAY)
    }
}

/// One worker writes the receipt before retiring its entry. A failed replacement of the queue
/// cannot erase this confirmation, even if the receiver immediately archives the recording.
pub fn save_receipt(path: &Path, entry: &QueuedCopy) -> Result<()> {
    storage::write_json(path, entry)
}

pub fn load_receipt(path: &Path) -> Result<Option<QueuedCopy>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| Error::msg(format!("cannot read recording copy receipt {}: {e}", path.display()))),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io("read recording copy receipt", path, e)),
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
    Same,
    Taken,
}

fn inspect(port: &dyn RecordingCopy, path: &Path, content: Content) -> io::Result<Destination> {
    match port.file_size(path) {
        Ok(size) if size == content.size && port.fingerprint(path)? == content => Ok(Destination::Same),
        Ok(_) => Ok(Destination::Taken),
        Err(e) if e.kind() == ErrorKind::IsADirectory => Ok(Destination::Taken),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Destination::Free),
        Err(e) => Err(e),
    }
}

/// A failed metadata lookup can also mean that the entire drive disappeared. A completed
/// directory listing distinguishes that from an absent filename (including broken links).
pub fn absent_from_parent(path: &Path) -> io::Result<bool> {
    let name = path.file_name().ok_or(ErrorKind::InvalidInput)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    for entry in fs::read_dir(parent)? {
        if entry?.file_name() == name {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn part_path(destination: &Path) -> PathBuf {
    let mut name = destination.as_os_str().to_owned();
    name.push(".part");
    name.into()
}

fn candidate(source: &Path, folder: &Path, number: u64) -> io::Result<PathBuf> {
    if number == 1 {
        return Ok(folder.join(source.file_name().ok_or(ErrorKind::InvalidInput)?));
    }
    let mut name = source.file_stem().ok_or(ErrorKind::InvalidInput)?.to_owned();
    name.push(format!("-{number}"));
    if let Some(ext) = source.extension() {
        name.push(".");
        name.push(ext);
    }
    Ok(folder.join(name))
}

/// False only for a verified collision. A negative lookup after AlreadyExists is inconclusive.
fn publish(port: &dyn RecordingCopy, part: &dyn RecordingPart, target: &Path, content: Content) -> io::Result<bool> {
    match part.publish(target) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => match inspect(port, target, content)? {
            Destination::Same => {
                part.discard()?;
                Ok(true)
            }
            Destination::Taken => Ok(false),
            Destination::Free => Err(e),
        },
        Err(e) => Err(e),
    }
}

fn send(
    port: &dyn RecordingCopy,
    entry: &mut QueuedCopy,
    folder: &Path,
    reserved: &HashMap<PathBuf, Content>,
    persist: &mut impl FnMut(&QueuedCopy) -> io::Result<()>,
) -> io::Result<()> {
    let content = match entry.recording.identity {
        Identity::Content(content) => content,
        Identity::Unidentified(_) => {
            let (content, modified) = port.identify_source(&entry.recording.source)?;
            entry.recording.identity = Identity::Content(content);
            entry.source_check = modified.map(SourceCheck::matched);
            persist(entry)?;
            content
        }
    };

    let mut staged = None;
    if let Some(handoff) = &entry.handoff {
        let Some(path) = &handoff.part else { return Ok(()) }; // Already verified at its final name.
        if path.parent() != handoff.target.parent()
            || path.extension() != Some("part".as_ref())
            || path == &entry.recording.source
        {
            return Err(io::Error::new(ErrorKind::InvalidData, "invalid queued recording part"));
        }
        let destination = inspect(port, &handoff.target, content)?;
        if destination == Destination::Same {
            return Ok(());
        }
        // Never reconstruct a committed part from the source. It may have been renamed and
        // consumed before a crash. Only the verified, still-unpublished bytes can be renamed.
        let part = port.reopen_part(path, content).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("cannot confirm delivery to {}: {e}", handoff.target.display()),
            )
        })?;
        if handoff.target.parent() == Some(folder)
            && destination == Destination::Free
            && publish(port, part.as_ref(), &handoff.target, content)?
        {
            return Ok(());
        }
        staged = Some((path.clone(), part));
    }

    for number in 1_u64.. {
        let target = candidate(&entry.recording.source, folder, number)?;
        let path = part_path(&target);
        match inspect(port, &target, content)? {
            Destination::Taken => continue,
            Destination::Same => {
                entry.handoff = Some(Handoff { target, part: None });
                persist(entry)?;
                if let Some((_, part)) = staged {
                    part.discard()?;
                }
                return Ok(());
            }
            Destination::Free => {}
        }

        if let Some(other) = reserved.get(&target).or_else(|| reserved.get(&path)) {
            if other == &content {
                return Err(ErrorKind::ResourceBusy.into());
            }
            continue;
        }

        // On a settings change transfer a prepared part to the newly selected folder. Keep
        // the old bytes until their replacement and its name are durable.
        let (path, part, old_part) = match staged.take() {
            Some((previous, part)) if previous.parent() == Some(folder) => (previous, part, None),
            Some((previous, old)) => {
                let part = port.stage(&previous, &path, content)?;
                (path, part, Some((previous, old)))
            }
            None => {
                let previous = entry.source_check;
                let part = port.stage_source(&entry.recording.source, &path, content, &mut entry.source_check);
                if part.is_err() && entry.source_check != previous {
                    persist(entry)?;
                }
                (path, part?, None)
            }
        };
        entry.handoff = Some(Handoff {
            target: target.clone(),
            part: Some(path.clone()),
        });
        persist(entry)?;
        if let Some((_, old)) = old_part {
            old.discard()?;
        }
        if publish(port, part.as_ref(), &target, content)? {
            return Ok(());
        }
        staged = Some((path, part));
    }
    Err(io::Error::other("no recording filename available"))
}

pub fn deliver_queued(
    port: &dyn RecordingCopy,
    entry: &mut QueuedCopy,
    folder: &Path,
    reserved: &HashMap<PathBuf, Content>,
    mut persist: impl FnMut(&QueuedCopy) -> io::Result<()>,
) -> io::Result<Delivery> {
    match send(port, entry, folder, reserved, &mut persist) {
        Ok(()) => Ok(Delivery::Sent),
        Err(error) => {
            let source = &entry.recording.source;
            if port.file_size(source).is_err_and(|e| e.kind() == ErrorKind::NotFound) && port.source_absent(source)? {
                return Ok(Delivery::SourceGone);
            }
            Err(error)
        }
    }
}

/// One-shot delivery for callers without a persistent queue.
pub fn deliver(port: &dyn RecordingCopy, source: &Path, folder: &Path) -> io::Result<Delivery> {
    let mut entry = QueuedCopy::new(Recording::new(source, None));
    deliver_queued(port, &mut entry, folder, &HashMap::new(), |_| Ok(()))
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

        fn fingerprint(&self, path: &Path) -> io::Result<Content> {
            Content::read(io::repeat(1).take(self.file_size(path)?))
        }

        fn stage(&self, _: &Path, _: &Path, _: Content) -> io::Result<Box<dyn RecordingPart + '_>> {
            Ok(Box::new(self))
        }

        fn reopen_part(&self, _: &Path, _: Content) -> io::Result<Box<dyn RecordingPart + '_>> {
            Ok(Box::new(self))
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

    impl RecordingPart for &OtherWriter {
        fn publish(&self, destination: &Path) -> io::Result<()> {
            RecordingCopy::publish(*self, Path::new("local/call.m4a"), destination)
        }

        fn discard(&self) -> io::Result<()> {
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
        let a = Recording::new(Path::new("first.m4a"), Some(Content::read(&b"one"[..]).unwrap()));
        let b = Recording::new(Path::new("second.m4a"), Some(Content::read(&b"two"[..]).unwrap()));
        let mut queue = CopyQueue::load(&path).unwrap();
        assert_eq!(queue.retry_after(), None);
        queue.enqueue(a.clone());
        queue.enqueue(a.clone());
        queue.enqueue(b.clone());
        queue.save(&path).unwrap();
        let mut reloaded = CopyQueue::load(&path).unwrap();
        assert_eq!(
            reloaded.pending().iter().map(|e| &e.recording).collect::<Vec<_>>(),
            [&a, &b]
        );
        assert_eq!(reloaded.retry_after(), Some(Duration::from_secs(300)));
        reloaded.finish(&b);
        assert_eq!(reloaded.pending().len(), 1);
        assert_eq!(reloaded.pending()[0].recording, a);
        reloaded.finish(&a);
        reloaded.save(&path).unwrap();
        assert_eq!(CopyQueue::load(&path).unwrap().retry_after(), None);
    }
}
