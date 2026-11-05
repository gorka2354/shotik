//! HTTP for translation over WinHTTP: the system's proxy settings apply, as they did under Chromium,
//! and the binary carries no TLS stack of its own. Implements `Http`.
//!
//! Errors name the failing step and the Windows error, never the URL or body: the query holds the text
//! being translated, and these messages reach logs and toasts.

use shotik_core::ports::{Http, HttpRequest, HttpResponse};
use shotik_core::{Error, Result};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
    WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

/// Bounds what a hostile server can make this process hold. It must stay well above the size of an installer: a limit
/// below it could only be fixed by the update it would block.
const MAX_RESPONSE: usize = 128 * 1024 * 1024;

/// A WinHTTP handle closed on drop.
struct Handle(*mut core::ffi::c_void);

impl Handle {
    fn new(raw: *mut core::ffi::c_void, step: &str) -> Result<Self> {
        if raw.is_null() {
            Err(failed(step))
        } else {
            Ok(Self(raw))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from WinHTTP, is non-null, and is closed exactly once here.
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

fn failed(step: &str) -> Error {
    Error::msg(format!("HTTP {step} failed: {}", windows::core::Error::from_thread().code()))
}

struct Target {
    secure: bool,
    host: String,
    port: u16,
    path_and_query: String,
}

fn parse_url(url: &str) -> Result<Target> {
    let invalid = || Error::msg("HTTP request has an invalid address");
    let (scheme, rest) = url.split_once("://").ok_or_else(invalid)?;
    let secure = match scheme {
        "https" => true,
        "http" => false,
        _ => return Err(invalid()),
    };
    let (authority, path_and_query) = match rest.find(['/', '?']) {
        Some(at) => (&rest[..at], rest[at..].to_string()),
        None => (rest, "/".to_string()),
    };
    let path_and_query = if path_and_query.starts_with('?') { format!("/{path_and_query}") } else { path_and_query };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().map_err(|_| invalid())?),
        None => (authority, if secure { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(invalid());
    }
    Ok(Target { secure, host: host.to_string(), port, path_and_query })
}

#[derive(Default)]
pub struct WinHttp;

impl Http for WinHttp {
    fn send(&self, request: HttpRequest<'_>) -> Result<HttpResponse> {
        let target = parse_url(request.url)?;
        let timeout = i32::try_from(request.timeout_ms).unwrap_or(i32::MAX);

        // SAFETY: every pointer below is valid for the call: HSTRINGs and the body outlive their
        // calls, and handles are closed by their guards after the calls that use them.
        unsafe {
            let session = Handle::new(
                WinHttpOpen(
                    &HSTRING::from("Mozilla/5.0"),
                    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    0,
                ),
                "session",
            )?;
            WinHttpSetTimeouts(session.0, timeout, timeout, timeout, timeout).map_err(|_| failed("timeout setup"))?;
            let connection =
                Handle::new(WinHttpConnect(session.0, &HSTRING::from(target.host.as_str()), target.port, 0), "connect")?;
            let flags = if target.secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
            let call = Handle::new(
                WinHttpOpenRequest(
                    connection.0,
                    &HSTRING::from(request.method),
                    &HSTRING::from(target.path_and_query.as_str()),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    std::ptr::null(),
                    flags,
                ),
                "request",
            )?;

            let headers: String = request.headers.iter().map(|(name, value)| format!("{name}: {value}\r\n")).collect();
            let headers: Vec<u16> = headers.encode_utf16().collect();
            let body = request.body.unwrap_or_default();
            let body_ptr = (!body.is_empty()).then_some(body.as_ptr().cast());
            let body_len = u32::try_from(body.len()).map_err(|_| Error::msg("HTTP request body is too large"))?;
            // An empty slice would pass a dangling pointer, which WinHTTP reads: no headers means null.
            let headers = (!headers.is_empty()).then_some(headers.as_slice());
            WinHttpSendRequest(call.0, headers, body_ptr, body_len, body_len, 0).map_err(|_| failed("send"))?;
            WinHttpReceiveResponse(call.0, std::ptr::null_mut()).map_err(|_| failed("receive"))?;

            let mut status = 0u32;
            let mut status_size = std::mem::size_of::<u32>() as u32;
            WinHttpQueryHeaders(
                call.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some(std::ptr::from_mut(&mut status).cast()),
                &mut status_size,
                std::ptr::null_mut(),
            )
            .map_err(|_| failed("status"))?;

            let mut out = Vec::new();
            loop {
                let mut available = 0u32;
                WinHttpQueryDataAvailable(call.0, &mut available).map_err(|_| failed("read"))?;
                if available == 0 {
                    break;
                }
                let start = out.len();
                if start + available as usize > MAX_RESPONSE {
                    return Err(Error::msg("HTTP response is too large"));
                }
                out.resize(start + available as usize, 0);
                let mut read = 0u32;
                WinHttpReadData(call.0, out[start..].as_mut_ptr().cast(), available, &mut read).map_err(|_| failed("read"))?;
                out.truncate(start + read as usize);
                if read == 0 {
                    break;
                }
            }
            Ok(HttpResponse { status: u16::try_from(status).unwrap_or(0), body: out })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    use super::*;

    /// A local server: tests never leave this machine.
    fn serve_once(reply: &'static str) -> (u16, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sent, seen) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let n = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..n]);
                let text = String::from_utf8_lossy(&request);
                if let Some(at) = text.find("\r\n\r\n") {
                    let length = text
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_string))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if request.len() >= at + 4 + length || n == 0 {
                        break;
                    }
                }
            }
            sent.send(String::from_utf8_lossy(&request).into_owned()).unwrap();
            stream.write_all(reply.as_bytes()).unwrap();
        });
        (port, seen)
    }

    fn request<'a>(method: &'a str, url: &'a str, headers: &'a [(&'a str, &'a str)], body: Option<&'a [u8]>) -> HttpRequest<'a> {
        HttpRequest { method, url, headers, body, timeout_ms: 5000 }
    }

    #[test]
    fn get_carries_the_query_and_headers_and_reads_the_reply() {
        let (port, seen) = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello");
        let url = format!("http://127.0.0.1:{port}/translate?q=%D0%BF%D1%80%D0%B8%D0%B2%D0%B5%D1%82&tl=ru");
        let reply = WinHttp.send(request("GET", &url, &[("Accept", "application/json")], None)).unwrap();
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, b"hello");
        let got = seen.recv().unwrap();
        assert!(got.starts_with("GET /translate?q=%D0%BF%D1%80%D0%B8%D0%B2%D0%B5%D1%82&tl=ru HTTP/1.1\r\n"), "{got}");
        assert!(got.contains("Accept: application/json\r\n"), "{got}");
    }

    #[test]
    fn post_sends_the_body() {
        let (port, seen) = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        let url = format!("http://127.0.0.1:{port}/v2/translate");
        let reply = WinHttp
            .send(request("POST", &url, &[("Content-Type", "application/x-www-form-urlencoded")], Some(b"text=hi&target_lang=RU")))
            .unwrap();
        assert_eq!(reply.body, b"ok");
        assert!(seen.recv().unwrap().ends_with("text=hi&target_lang=RU"));
    }

    #[test]
    fn an_http_error_status_is_a_response_not_a_failure() {
        let (port, _seen) = serve_once("HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let reply = WinHttp.send(request("GET", &format!("http://127.0.0.1:{port}/"), &[], None)).unwrap();
        assert_eq!(reply.status, 429);
    }

    #[test]
    fn a_failure_does_not_reveal_the_address_or_the_text() {
        // Nothing listens here: bind and drop to get a free port.
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/translate?q=secret-text");
        let error = WinHttp.send(request("GET", &url, &[], None)).err().expect("nothing listens").to_string();
        assert!(!error.contains("secret-text"), "{error}");
        assert!(!error.contains("127.0.0.1"), "{error}");
    }

    /// The kind of address an update downloads from, redirected to another host, at the size of a real installer. It needs the
    /// network, so it is left out of a normal run: `cargo test -p shotik-win --lib -- --ignored github_release`.
    #[test]
    #[ignore = "needs the network"]
    fn github_release_downloads_follow_redirects() {
        let url = "https://github.com/gorka2354/shotik/releases/download/v2.0.1/Shotik-Setup-2.0.1.exe";
        let reply = WinHttp.send(HttpRequest { method: "GET", url, headers: &[], body: None, timeout_ms: 60_000 }).unwrap();
        assert_eq!(reply.status, 200);
        assert!(reply.body.len() > 1024 * 1024, "{} bytes", reply.body.len());
        assert_eq!(&reply.body[..2], b"MZ", "an executable, not a page");
    }

    #[test]
    fn urls_parse_or_fail_clearly() {
        let t = parse_url("https://translate.googleapis.com/translate_a/single?q=1").unwrap();
        assert!(t.secure);
        assert_eq!((t.host.as_str(), t.port), ("translate.googleapis.com", 443));
        assert_eq!(t.path_and_query, "/translate_a/single?q=1");
        assert_eq!(parse_url("http://a.example?q=1").unwrap().path_and_query, "/?q=1");
        assert_eq!(parse_url("http://a.example").unwrap().path_and_query, "/");
        for bad in ["ftp://a.example/", "no-scheme", "http://", "http://a.example:99999/"] {
            let error = parse_url(bad).err().expect(bad).to_string();
            assert!(!error.contains("a.example"), "{error}");
        }
    }
}
