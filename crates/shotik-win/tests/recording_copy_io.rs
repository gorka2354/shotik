//! One synthetic recording in a separate process so unrelated reads cannot move the I/O counter.
#![cfg(windows)]

use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

use shotik_core::recording_copy::{deliver_queued, Content, CopyQueue, Delivery, QueuedCopy, Recording};
use shotik_win::recording_copy::WindowsRecordingCopy;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessIoCounters, IO_COUNTERS};

fn read_bytes() -> u64 {
    let mut counters = IO_COUNTERS::default();
    // SAFETY: the pseudo-handle is valid and the out-parameter is writable.
    unsafe { GetProcessIoCounters(GetCurrentProcess(), &mut counters).unwrap() };
    counters.ReadTransferCount
}

fn attempt(entry: &mut QueuedCopy, folder: &Path) -> io::Result<Delivery> {
    deliver_queued(&WindowsRecordingCopy, entry, folder, &Default::default(), |_| Ok(()))
}

#[test]
fn retries_only_read_the_source_to_copy_it_or_check_a_change() {
    let dir = std::env::temp_dir().join(format!("shotik-copy-io-{}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let source = dir.join("Rec.m4a");
    let bytes = vec![1; 4 * 1024 * 1024];
    fs::write(&source, &bytes).unwrap();
    let content = Content::read(bytes.as_slice()).unwrap();
    let folder = dir.join("calls");
    fs::create_dir(&folder).unwrap();
    let refused = folder.join("Rec.m4a.part");
    fs::create_dir(&refused).unwrap();
    let mut known = CopyQueue::default();
    known.enqueue(Recording::new(&source, Some(content)));
    let mut entry = known.pending()[0].clone();
    let before = read_bytes();
    assert!(attempt(&mut entry, &folder).is_err());
    assert_eq!(read_bytes() - before, 0, "refused part read the source");

    let recording = Recording::new(&source, None);
    let mut queue = CopyQueue::default();
    queue.enqueue(recording.clone());
    let mut entry = queue.pending()[0].clone();
    assert!(attempt(&mut entry, &folder).is_err());
    queue.update(&recording, &entry).unwrap();
    let queue_path = dir.join("queue.json");
    queue.save(&queue_path).unwrap();
    let stored = CopyQueue::load(&queue_path).unwrap().pending()[0].clone();

    let before = read_bytes();
    assert!(attempt(&mut stored.clone(), &folder).is_err());
    assert_eq!(
        read_bytes() - before,
        0,
        "refused part reread the identified source after restart"
    );
    fs::remove_dir(refused).unwrap();
    let before = read_bytes();
    assert_eq!(attempt(&mut stored.clone(), &folder).unwrap(), Delivery::Sent);
    assert!(
        read_bytes() - before <= bytes.len() as u64,
        "unchanged source was read again for hashing"
    );
    let destination = folder.join("Rec.m4a");
    assert_eq!(fs::read(&destination).unwrap(), bytes);
    fs::remove_file(destination).unwrap();

    // New bytes of the same size need a fresh verification before publication.
    fs::write(&source, vec![2; bytes.len()]).unwrap();
    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(86_400);
    fs::File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    let mut entry = stored;
    assert_eq!(
        attempt(&mut entry, &folder).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let before = read_bytes();
    assert_eq!(
        attempt(&mut entry, &folder).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(read_bytes() - before, 0, "unchanged replacement was hashed twice");
    fs::write(&source, b"shorter").unwrap();
    let before = read_bytes();
    assert_eq!(
        attempt(&mut entry, &folder).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(read_bytes() - before, 0, "a size mismatch needs only metadata");
    assert_eq!(fs::read_dir(&folder).unwrap().count(), 0);
    // Seeing a different size invalidates the earlier check, even if its timestamp is restored.
    fs::write(&source, &bytes).unwrap();
    fs::File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    assert_eq!(attempt(&mut entry, &folder).unwrap(), Delivery::Sent);
    assert_eq!(fs::read(folder.join("Rec.m4a")).unwrap(), bytes);
    fs::remove_dir_all(dir).unwrap();
}
