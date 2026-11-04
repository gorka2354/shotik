//! Per-user single instance and argument forwarding, independent of the UI.
//!
//! Call `acquire(test_mode, &std::env::args_os().collect::<Vec<_>>(), callback)`
//! before app startup. `Launch::Primary(guard)` owns the lock and pipe listener:
//! keep it until shutdown. `Launch::Forwarded` means the first instance received
//! the complete argument vector; exit without running startup. The application
//! interprets `--capture` / `--hidden`, just as the Electron second-instance handler
//! does. `test_mode` must include both `--test` and SHOTIK_TEST, as in main.js.
//!
//! The callback runs on the listener thread and must enqueue work to the app loop.
//! An ACK is sent only after it returns. UTF-16 framing preserves Windows argv,
//! including empty arguments, Unicode, quotes and unpaired surrogates. The pipe
//! rejects remote clients and has a DACL limited to the current user's SID.

use std::ffi::OsString;
use std::os::windows::{
    ffi::{OsStrExt, OsStringExt},
    io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use shotik_core::error::{Error, Result};
use shotik_core::ports::Callback;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Security::Authorization::*;
use windows::Win32::Security::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Pipes::*;
use windows::Win32::System::Threading::*;
use windows::Win32::System::IO::*;

const MAX_MESSAGE: usize = 1024 * 1024;
const IO_TIMEOUT_MS: u32 = 5000;

pub enum Launch {
    Primary(SingleInstance),
    Forwarded,
}

pub struct SingleInstance {
    // Released only after the pipe and listener have gone away.
    _lock: OwnedHandle,
    stop: Arc<OwnedHandle>,
    listener: Option<JoinHandle<()>>,
    error: Arc<Mutex<Option<Error>>>,
}

impl SingleInstance {
    /// Asynchronous listener/protocol failures. No argv contents are logged.
    pub fn take_error(&self) -> Option<Error> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

pub fn acquire(
    test_mode: bool,
    argv: &[OsString],
    forwarded: Callback<Vec<OsString>>,
) -> Result<Launch> {
    acquire_named(if test_mode { "test" } else { "app" }, argv, forwarded)
}

fn native_error(e: windows::core::Error) -> Error {
    Error::msg(format!("Single instance: {e}"))
}

fn raw(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}

fn owned(handle: HANDLE) -> Result<OwnedHandle> {
    if handle.is_invalid() {
        return Err(native_error(windows::core::Error::from_thread()));
    }
    // This function is only called with a newly acquired owning kernel handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle.0) })
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

struct LocalAllocation(*mut core::ffi::c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0)));
        }
    }
}

fn user_sid() -> Result<String> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .map_err(native_error)?;
    let token = owned(token)?;
    let mut bytes = 0;
    let queried = unsafe { GetTokenInformation(raw(&token), TokenUser, None, 0, &mut bytes) };
    if let Err(error) = queried {
        if error.code() != ERROR_INSUFFICIENT_BUFFER.to_hresult() {
            return Err(native_error(error));
        }
    }
    // TOKEN_USER requires pointer alignment; a byte Vec does not guarantee it.
    let mut storage = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
    unsafe {
        GetTokenInformation(
            raw(&token),
            TokenUser,
            Some(storage.as_mut_ptr().cast()),
            bytes,
            &mut bytes,
        )
    }
    .map_err(native_error)?;
    let user = unsafe { &*storage.as_ptr().cast::<TOKEN_USER>() };
    let mut sid = PWSTR::null();
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid) }.map_err(native_error)?;
    let _allocation = LocalAllocation(sid.0.cast());
    unsafe { sid.to_string() }.map_err(|e| Error::msg(format!("Invalid user SID string: {e}")))
}

fn security(sid: &str) -> Result<(LocalAllocation, SECURITY_ATTRIBUTES)> {
    let sddl = wide(&format!("D:P(A;;GA;;;{sid})"));
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .map_err(native_error)?;
    Ok((
        LocalAllocation(descriptor.0),
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        },
    ))
}

fn acquire_named(
    namespace: &str,
    argv: &[OsString],
    forwarded: Callback<Vec<OsString>>,
) -> Result<Launch> {
    let message = encode(argv)?;
    let sid = user_sid()?;
    let (_descriptor, security) = security(&sid)?;
    let mutex_name = wide(&format!("Global\\Shotik.{sid}.{namespace}"));
    let pipe_name = wide(&format!("\\\\.\\pipe\\Shotik.{sid}.{namespace}"));
    unsafe {
        SetLastError(ERROR_SUCCESS);
    }
    let mutex = unsafe { CreateMutexW(Some(&security), false, PCWSTR(mutex_name.as_ptr())) }
        .map_err(native_error)?;
    let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    let lock = owned(mutex)?;
    if exists {
        forward(&pipe_name, &message)?;
        return Ok(Launch::Forwarded);
    }
    let pipe = owned(unsafe {
        CreateNamedPipeW(
            PCWSTR(pipe_name.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            8192,
            8192,
            IO_TIMEOUT_MS,
            Some(&security),
        )
    })?;
    let stop = Arc::new(owned(
        unsafe { CreateEventW(None, true, false, None) }.map_err(native_error)?,
    )?);
    let listener_stop = Arc::clone(&stop);
    let error = Arc::new(Mutex::new(None));
    let listener_error = Arc::clone(&error);
    let (ready, initialized) = mpsc::sync_channel(1);
    let listener = thread::Builder::new()
        .name("shotik-instance".into())
        .spawn(move || {
            let pipe = raw(&pipe);
            let stop = raw(&listener_stop);
            let _ = ready.send(());
            loop {
                let connected = operation(pipe, stop, INFINITE, |overlapped, _| {
                    match unsafe { ConnectNamedPipe(pipe, Some(overlapped)) } {
                        Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => Ok(()),
                        result => result,
                    }
                });
                if let Err(e) = connected {
                    if unsafe { WaitForSingleObject(stop, 0) } != WAIT_OBJECT_0 {
                        *listener_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                    }
                    break;
                }
                let received = || -> Result<()> {
                    let deadline = Instant::now() + Duration::from_millis(u64::from(IO_TIMEOUT_MS));
                    let mut prefix = [0; 4];
                    read_exact(pipe, stop, &mut prefix, deadline)?;
                    let length = u32::from_le_bytes(prefix) as usize;
                    if !(4..=MAX_MESSAGE).contains(&length) {
                        return Err(Error::msg("Invalid argument frame length"));
                    }
                    let mut payload = vec![0; length];
                    read_exact(pipe, stop, &mut payload, deadline)?;
                    let argv = decode(&payload)?;
                    forwarded(argv);
                    write_all(pipe, stop, &[1], deadline)?;
                    // DisconnectNamedPipe discards unread buffered data. Wait for
                    // the client to confirm it read the ACK before disconnecting.
                    let mut confirmed = [0];
                    read_exact(pipe, stop, &mut confirmed, deadline)?;
                    if confirmed != [2] {
                        return Err(Error::msg("Invalid argument acknowledgement"));
                    }
                    Ok(())
                };
                if let Err(e) = received() {
                    if unsafe { WaitForSingleObject(stop, 0) } != WAIT_OBJECT_0 {
                        *listener_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                    }
                }
                if let Err(e) = unsafe { DisconnectNamedPipe(pipe) } {
                    *listener_error.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(native_error(e));
                    break;
                }
                if unsafe { WaitForSingleObject(stop, 0) } == WAIT_OBJECT_0 {
                    break;
                }
            }
        })
        .map_err(|e| Error::msg(format!("Cannot start single-instance listener: {e}")))?;
    if initialized.recv().is_err() {
        let _ = listener.join();
        return Err(Error::msg("Single-instance listener exited during startup"));
    }
    Ok(Launch::Primary(SingleInstance {
        _lock: lock,
        stop,
        listener: Some(listener),
        error,
    }))
}

fn forward(name: &[u16], message: &[u8]) -> Result<()> {
    let deadline = Instant::now() + Duration::from_millis(u64::from(IO_TIMEOUT_MS));
    let pipe = loop {
        match unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                None,
            )
        } {
            Ok(handle) => break owned(handle)?,
            Err(e)
                if e.code() == ERROR_PIPE_BUSY.to_hresult()
                    || e.code() == ERROR_FILE_NOT_FOUND.to_hresult() =>
            {
                let timeout = remaining(deadline)?;
                unsafe {
                    let _ = WaitNamedPipeW(PCWSTR(name.as_ptr()), timeout.min(50));
                }
                // WaitNamedPipe returns immediately when the first instance is
                // still starting. Do not spin while waiting for its endpoint.
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(native_error(e)),
        }
    };
    let stop = owned(unsafe { CreateEventW(None, true, false, None) }.map_err(native_error)?)?;
    write_all(raw(&pipe), raw(&stop), message, deadline)?;
    let mut ack = [0];
    read_exact(raw(&pipe), raw(&stop), &mut ack, deadline)?;
    if ack != [1] {
        return Err(Error::msg(
            "First instance did not acknowledge the argument vector",
        ));
    }
    write_all(raw(&pipe), raw(&stop), &[2], deadline)?;
    Ok(())
}

fn remaining(deadline: Instant) -> Result<u32> {
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .as_millis();
    if remaining == 0 {
        Err(Error::msg("Single-instance pipe timed out"))
    } else {
        Ok(remaining.min(u128::from(u32::MAX - 1)) as u32)
    }
}

fn operation(
    pipe: HANDLE,
    stop: HANDLE,
    timeout: u32,
    start: impl FnOnce(*mut OVERLAPPED, &mut u32) -> windows::core::Result<()>,
) -> Result<u32> {
    let event = owned(unsafe { CreateEventW(None, true, false, None) }.map_err(native_error)?)?;
    let mut overlapped = OVERLAPPED {
        hEvent: raw(&event),
        ..OVERLAPPED::default()
    };
    let mut bytes = 0;
    match start(&mut overlapped, &mut bytes) {
        Ok(()) => return Ok(bytes),
        Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {}
        Err(e) => return Err(native_error(e)),
    }
    let waited = unsafe { WaitForMultipleObjects(&[stop, raw(&event)], false, timeout) };
    if waited.0 == WAIT_OBJECT_0.0 + 1 {
        unsafe { GetOverlappedResult(pipe, &overlapped, &mut bytes, false) }
            .map_err(native_error)?;
        return Ok(bytes);
    }
    // Do not release OVERLAPPED or its buffers until cancellation has completed.
    unsafe {
        let _ = CancelIoEx(pipe, Some(&overlapped));
        let _ = GetOverlappedResult(pipe, &overlapped, &mut bytes, true);
    }
    if waited == WAIT_OBJECT_0 {
        Err(Error::msg("Single-instance listener stopped"))
    } else if waited == WAIT_TIMEOUT {
        Err(Error::msg("Single-instance pipe timed out"))
    } else {
        Err(native_error(windows::core::Error::from_thread()))
    }
}

fn read_exact(pipe: HANDLE, stop: HANDLE, mut buffer: &mut [u8], deadline: Instant) -> Result<()> {
    while !buffer.is_empty() {
        let bytes = operation(
            pipe,
            stop,
            remaining(deadline)?,
            |overlapped, bytes| unsafe {
                ReadFile(pipe, Some(buffer), Some(bytes), Some(overlapped))
            },
        )? as usize;
        if bytes == 0 {
            return Err(Error::msg(
                "Single-instance pipe closed before the complete argument vector",
            ));
        }
        buffer = &mut buffer[bytes..];
    }
    Ok(())
}

fn write_all(pipe: HANDLE, stop: HANDLE, mut buffer: &[u8], deadline: Instant) -> Result<()> {
    while !buffer.is_empty() {
        let bytes = operation(
            pipe,
            stop,
            remaining(deadline)?,
            |overlapped, bytes| unsafe {
                WriteFile(pipe, Some(buffer), Some(bytes), Some(overlapped))
            },
        )? as usize;
        if bytes == 0 {
            return Err(Error::msg("Single-instance pipe accepted no data"));
        }
        buffer = &buffer[bytes..];
    }
    Ok(())
}

fn encode(argv: &[OsString]) -> Result<Vec<u8>> {
    if argv.len() > MAX_MESSAGE / 4 {
        return Err(Error::msg("Too many forwarded arguments"));
    }
    let mut message = vec![0; 4];
    message.extend_from_slice(&(argv.len() as u32).to_le_bytes());
    for argument in argv {
        let words: Vec<_> = argument.encode_wide().collect();
        if words.len() > MAX_MESSAGE / 2 || message.len() + 4 + words.len() * 2 > MAX_MESSAGE + 4 {
            return Err(Error::msg(
                "Forwarded arguments exceed the pipe message limit",
            ));
        }
        message.extend_from_slice(&(words.len() as u32).to_le_bytes());
        for word in words {
            message.extend_from_slice(&word.to_le_bytes());
        }
    }
    let length = (message.len() - 4) as u32;
    message[..4].copy_from_slice(&length.to_le_bytes());
    Ok(message)
}

fn decode(mut bytes: &[u8]) -> Result<Vec<OsString>> {
    fn number(bytes: &mut &[u8]) -> Result<usize> {
        if bytes.len() < 4 {
            return Err(Error::msg("Truncated argument frame"));
        }
        let value = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        *bytes = &bytes[4..];
        Ok(value)
    }
    if bytes.len() > MAX_MESSAGE {
        return Err(Error::msg("Argument frame exceeds the message limit"));
    }
    let count = number(&mut bytes)?;
    if count > bytes.len() / 4 {
        return Err(Error::msg("Invalid argument count"));
    }
    let mut argv = Vec::with_capacity(count);
    for _ in 0..count {
        let words = number(&mut bytes)?;
        if words > bytes.len() / 2 {
            return Err(Error::msg("Truncated UTF-16 argument"));
        }
        let text: Vec<_> = bytes[..words * 2]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        argv.push(OsString::from_wide(&text));
        bytes = &bytes[words * 2..];
    }
    if !bytes.is_empty() {
        return Err(Error::msg("Trailing bytes in argument frame"));
    }
    Ok(argv)
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(raw(&self.stop));
        }
        if let Some(listener) = self.listener.take() {
            if listener.thread().id() != thread::current().id() {
                let _ = listener.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn namespace() -> String {
        static SEQUENCE: AtomicU32 = AtomicU32::new(0);
        format!(
            "r4-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn frame_preserves_windows_arguments_and_rejects_bad_input() {
        let argv = vec![
            "shotik.exe".into(),
            "--capture=region".into(),
            "".into(),
            "путь с пробелами\\\"🦀".into(),
            OsString::from_wide(&[0xd800]),
        ];
        let message = encode(&argv).unwrap();
        assert_eq!(decode(&message[4..]).unwrap(), argv);
        for bad in [
            &[][..],
            &[1, 0, 0, 0][..],
            &[0, 0, 0, 0, 1][..],
            &[1, 0, 0, 0, 255, 255, 255, 255][..],
        ] {
            assert!(decode(bad).is_err());
        }
        assert!(encode(&[OsString::from("x".repeat(MAX_MESSAGE))]).is_err());
    }

    #[test]
    fn forwards_to_first_instance_and_releases_lock_on_drop() {
        let namespace = namespace();
        let (sent, received) = mpsc::channel();
        let callback: Callback<Vec<OsString>> = Arc::new(move |argv| {
            sent.send(argv).unwrap();
        });
        let first = acquire_named(&namespace, &[], Arc::clone(&callback)).unwrap();
        let Launch::Primary(first) = first else {
            panic!("expected primary");
        };
        for args in [
            vec!["shotik.exe".into(), "--capture".into(), "audio".into()],
            vec!["shotik.exe".into()],
            vec![
                "shotik.exe".into(),
                "--capture=translate".into(),
                "русский путь".into(),
            ],
        ] {
            assert!(matches!(
                acquire_named(&namespace, &args, Arc::clone(&callback)).unwrap(),
                Launch::Forwarded
            ));
            assert_eq!(received.recv_timeout(Duration::from_secs(2)).unwrap(), args);
        }
        assert!(first.take_error().is_none());
        let start = Instant::now();
        drop(first);
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(matches!(
            acquire_named(&namespace, &[], callback).unwrap(),
            Launch::Primary(_)
        ));
    }

    #[test]
    fn separate_namespaces_do_not_forward_to_each_other() {
        let namespace = namespace();
        let callback: Callback<Vec<OsString>> = Arc::new(|_| panic!("independent namespace"));
        let a = acquire_named(&format!("{namespace}-app"), &[], Arc::clone(&callback)).unwrap();
        let b = acquire_named(&format!("{namespace}-test"), &[], callback).unwrap();
        assert!(matches!(a, Launch::Primary(_)));
        assert!(matches!(b, Launch::Primary(_)));
    }

    #[test]
    fn forwards_from_another_process_without_a_console_window() {
        const CHILD_NAMESPACE: &str = "SHOTIK_R4_INSTANCE_CHILD_NAMESPACE";
        let arguments = vec![
            "shotik.exe".into(),
            "--capture=region".into(),
            "путь с пробелами".into(),
            "".into(),
        ];
        if let Some(namespace) = std::env::var_os(CHILD_NAMESPACE) {
            let callback: Callback<Vec<OsString>> =
                Arc::new(|_| panic!("child must not become primary"));
            assert!(matches!(
                acquire_named(&namespace.to_string_lossy(), &arguments, callback).unwrap(),
                Launch::Forwarded
            ));
            return;
        }
        let namespace = namespace();
        let (sent, received) = mpsc::channel();
        let callback: Callback<Vec<OsString>> = Arc::new(move |argv| {
            sent.send(argv).unwrap();
        });
        let Launch::Primary(primary) = acquire_named(&namespace, &[], callback).unwrap() else {
            panic!("expected primary");
        };
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "single_instance::tests::forwards_from_another_process_without_a_console_window",
                "--nocapture",
            ])
            .env(CHILD_NAMESPACE, &namespace)
            .creation_flags(CREATE_NO_WINDOW.0)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stderr)
        );
        assert_eq!(
            received.recv_timeout(Duration::from_secs(2)).unwrap(),
            arguments
        );
        assert!(primary.take_error().is_none());
    }

    #[test]
    fn dropping_listener_cancels_an_incomplete_client_read() {
        let namespace = namespace();
        let callback: Callback<Vec<OsString>> =
            Arc::new(|_| panic!("incomplete argv must not reach handler"));
        let Launch::Primary(primary) = acquire_named(&namespace, &[], callback).unwrap() else {
            panic!("expected primary");
        };
        let pipe_name = wide(&format!(
            "\\\\.\\pipe\\Shotik.{}.{}",
            user_sid().unwrap(),
            namespace
        ));
        let client = owned(
            unsafe {
                CreateFileW(
                    PCWSTR(pipe_name.as_ptr()),
                    (GENERIC_READ | GENERIC_WRITE).0,
                    FILE_SHARE_MODE(0),
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                    None,
                )
            }
            .unwrap(),
        )
        .unwrap();
        // Connect but send no frame. Shutdown must cancel the pending read rather
        // than wait for the five-second client deadline or terminate a process.
        let started = Instant::now();
        drop(primary);
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(client);
    }
}
