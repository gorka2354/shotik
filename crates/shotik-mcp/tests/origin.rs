//! A web page in the user's browser must not reach the MCP server: not by POSTing to localhost, and
//! not through DNS rebinding, where its own domain resolves to 127.0.0.1 and it can read the reply.

mod common;

use common::Fixture;
use shotik_mcp::is_local_request;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[test]
fn only_loopback_hosts_pass() {
    for host in ["127.0.0.1:7464", "localhost:7464", "LOCALHOST:7464", "[::1]:7464", "127.0.0.1", "localhost"] {
        assert!(is_local_request(Some(host), None), "{host}");
    }
    for host in ["evil.example:7464", "evil.example", "127.0.0.1.evil.example", "localhost.evil.example:7464", "", "[::2]:7464"] {
        assert!(!is_local_request(Some(host), None), "{host}");
    }
    assert!(!is_local_request(None, None), "HTTP/1.1 requires Host");
}

#[test]
fn an_origin_must_be_loopback_too() {
    let host = Some("127.0.0.1:7464");
    for origin in ["http://localhost:3000", "http://127.0.0.1:6274", "https://localhost", "http://[::1]:8080"] {
        assert!(is_local_request(host, Some(origin)), "{origin}");
    }
    for origin in [
        "http://evil.example",
        "https://evil.example:7464",
        "http://localhost.evil.example",
        "http://127.0.0.1.evil.example",
        "null",
        "file://",
        "chrome-extension://abc",
        "",
    ] {
        assert!(!is_local_request(host, Some(origin)), "{origin}");
    }
}

/// A raw request, because HTTP clients refuse to fake Host.
async fn status_of(port: u16, extra_headers: &str) -> String {
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let request = format!(
        "POST /mcp HTTP/1.1\r\n{extra_headers}Content-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await.unwrap();
    reply.lines().next().unwrap_or_default().to_string()
}

#[tokio::test]
async fn the_server_refuses_a_page_and_serves_a_local_client() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let port = server.start(0).await.unwrap();

    let local = status_of(port, &format!("Host: 127.0.0.1:{port}\r\n")).await;
    assert!(local.contains("200"), "local client: {local}");

    let rebinding = status_of(port, &format!("Host: evil.example:{port}\r\nOrigin: http://evil.example:{port}\r\n")).await;
    assert!(rebinding.contains("403"), "DNS rebinding: {rebinding}");

    let cross_site = status_of(port, &format!("Host: 127.0.0.1:{port}\r\nOrigin: https://evil.example\r\n")).await;
    assert!(cross_site.contains("403"), "cross-site POST: {cross_site}");

    server.stop().await.unwrap();
}
