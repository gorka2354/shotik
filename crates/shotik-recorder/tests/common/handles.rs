//! Handles of this process by kernel object type, to see what a leak is made of.
#![allow(dead_code)]

#[repr(C)]
struct HandleEntry {
    object: *mut core::ffi::c_void,
    pid: usize,
    handle: usize,
    access: u32,
    backtrace: u16,
    type_index: u16,
    attributes: u32,
    reserved: u32,
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum: u16,
    buffer: *const u16,
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQuerySystemInformation(class: u32, info: *mut core::ffi::c_void, len: u32, ret: *mut u32) -> i32;
    fn NtQueryObject(handle: isize, class: u32, info: *mut core::ffi::c_void, len: u32, ret: *mut u32) -> i32;
}

/// How many handles of each kernel object type this process holds, to see what a leak is made of.
pub fn handle_types() -> std::collections::BTreeMap<String, i64> {
    const SYSTEM_EXTENDED_HANDLE_INFORMATION: u32 = 64;
    const OBJECT_TYPE_INFORMATION: u32 = 2;
    const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
    let mut buffer = vec![0_u64; 4 * 1024 * 1024];
    loop {
        let mut ret = 0;
        // SAFETY: the buffer is ours and its length is passed.
        let status = unsafe {
            NtQuerySystemInformation(SYSTEM_EXTENDED_HANDLE_INFORMATION, buffer.as_mut_ptr().cast(), (buffer.len() * 8) as u32, &mut ret)
        };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        assert!(status >= 0, "NtQuerySystemInformation {status:#x}");
        break;
    }
    let pid = std::process::id() as usize;
    let count = buffer[0] as usize;
    // SAFETY: the layout of SYSTEM_HANDLE_INFORMATION_EX: two pointer-sized fields, then the entries.
    let entries = unsafe { std::slice::from_raw_parts(buffer.as_ptr().add(2).cast::<HandleEntry>(), count) };
    let mut names: std::collections::BTreeMap<u16, String> = std::collections::BTreeMap::new();
    let mut counts = std::collections::BTreeMap::new();
    let mut info = vec![0_u64; 1024];
    for entry in entries.iter().filter(|e| e.pid == pid) {
        let name = names.entry(entry.type_index).or_insert_with(|| {
            let mut ret = 0;
            // SAFETY: a handle of this process and a buffer of ours. This information class does not
            // block, unlike the name query.
            let status = unsafe {
                NtQueryObject(entry.handle as isize, OBJECT_TYPE_INFORMATION, info.as_mut_ptr().cast(), (info.len() * 8) as u32, &mut ret)
            };
            if status < 0 {
                return format!("type {}", entry.type_index);
            }
            // SAFETY: on success the buffer starts with the type name.
            unsafe {
                let text = &*info.as_ptr().cast::<UnicodeString>();
                String::from_utf16_lossy(std::slice::from_raw_parts(text.buffer, usize::from(text.length) / 2))
            }
        });
        *counts.entry(name.clone()).or_insert(0) += 1;
    }
    counts
}

pub fn diff_types(
    before: &std::collections::BTreeMap<String, i64>,
    after: &std::collections::BTreeMap<String, i64>,
) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for (name, count) in after {
        let delta = count - before.get(name).copied().unwrap_or(0);
        if delta != 0 {
            out.push((name.clone(), delta));
        }
    }
    for (name, count) in before {
        if !after.contains_key(name) {
            out.push((name.clone(), -count));
        }
    }
    out
}
