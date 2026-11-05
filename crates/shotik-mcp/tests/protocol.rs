mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use serde_json::{json, Value};
use shotik_core::Error;
use shotik_mcp::{CallLogEntry, ServerStatus, BODY_LIMIT};
use tokio::net::TcpListener;
use tokio::time::timeout;
use uuid::Uuid;

#[tokio::test]
async fn http_methods_paths_and_stateless_sessions() {
    let fixture = Fixture::new();
    let server = fixture.server();
    assert_eq!(
        server.status(),
        ServerStatus {
            running: false,
            port: None
        }
    );
    let port = server.start(0).await.unwrap();
    assert_ne!(port, 0);
    assert_eq!(
        server.status(),
        ServerStatus {
            running: true,
            port: Some(port)
        }
    );
    assert_eq!(server.start(0).await.unwrap(), port);
    for method in ["GET", "PUT", "PATCH", "HEAD", "OPTIONS"] {
        let response = request(port, method, "/mcp", b"").await;
        assert_eq!(response.status, 405, "{method}");
        assert_eq!(response.headers["allow"], "POST, DELETE");
        assert!(response.body.is_empty());
    }
    let response = request(port, "DELETE", "/mcp", b"").await;
    assert_eq!(response.status, 200);
    assert!(response.body.is_empty());
    for path in ["/", "/mcp/", "/test/state"] {
        let response = request(port, "POST", path, b"{}").await;
        assert_eq!(response.status, 404);
        assert_eq!(response.headers["content-type"], "application/json");
        assert_eq!(response.json(), json!({"error":"not found"}));
    }
    // No session or initialize is required by the stateless reference server.
    let response = request(
        port,
        "POST",
        "/mcp?client=x",
        br#"{"id":0,"method":"ping"}"#,
    )
    .await;
    assert_eq!(response.json(), json!({"jsonrpc":"2.0","id":0,"result":{}}));
    server.stop().await.unwrap();
    assert_eq!(
        server.status(),
        ServerStatus {
            running: false,
            port: None
        }
    );
    server.stop().await.unwrap();
    let port = server.start(port).await.unwrap();
    assert_eq!(rpc(port, json!({"id":1,"method":"ping"})).await.status, 200);
    server.stop().await.unwrap();
}

#[tokio::test]
async fn loopback_only_and_bind_errors_do_not_leave_running_state() {
    let fixture = Fixture::new();
    let occupied = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let server = fixture.server();
    assert!(server
        .start(occupied.local_addr().unwrap().port())
        .await
        .is_err());
    assert!(!server.status().running);
    let port = server.start(0).await.unwrap();
    // The same port remains available on a different loopback address: the
    // server did not bind INADDR_ANY (0.0.0.0).
    let other_interface = TcpListener::bind(("127.0.0.2", port)).await.unwrap();
    assert_eq!(
        other_interface.local_addr().unwrap().ip().to_string(),
        "127.0.0.2"
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn initialize_echo_fallback_version_and_session_header() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    let mut previous_session = String::new();
    for params in [
        json!({"protocolVersion":"2024-11-05"}),
        json!({"protocolVersion":"2030-custom"}),
        json!({}),
        json!(null),
        json!({"protocolVersion":""}),
    ] {
        let expected = params["protocolVersion"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("2025-03-26");
        let response = rpc(
            port,
            json!({"id":"init","method":"initialize","params":params}),
        )
        .await;
        assert_eq!(response.status, 200);
        assert_eq!(
            response.json(),
            json!({"jsonrpc":"2.0","id":"init","result":{
                "protocolVersion":expected,
                "capabilities":{"tools":{"listChanged":false}},
                "serverInfo":{"name":"shotik","title":"Shotik Screenshots","version":"test-server-version"},
                "instructions":"Screenshot tools for the user's screen. Use take_screenshot to see the screen, ask_user_to_select_region to let the user mark a specific area.",
            }})
        );
        let session = response.headers.get("mcp-session-id").unwrap();
        assert_eq!(Uuid::parse_str(session).unwrap().get_version_num(), 4);
        assert_ne!(session, &previous_session);
        previous_session = session.clone();
    }
    let ping = rpc(port, json!({"id":1,"method":"ping"})).await;
    assert!(!ping.headers.contains_key("mcp-session-id"));
    assert_eq!(ping.json(), json!({"jsonrpc":"2.0","id":1,"result":{}}));
    // Known methods answer even without id, just as handleRpc does in JS.
    let response = rpc(port, json!({"method":"initialize"})).await;
    assert_eq!(response.status, 200);
    assert!(response.headers.contains_key("mcp-session-id"));
    assert!(response.json().get("id").is_none());
    server.stop().await.unwrap();
}

#[tokio::test]
async fn parse_method_tool_errors_and_notifications() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    for body in [b"".as_slice(), b"{", b"not json", &[0xff]] {
        let response = request(port, "POST", "/mcp", body).await;
        assert_eq!(response.status, 400);
        assert_eq!(
            response.json(),
            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}})
        );
    }
    for id in [json!(9), json!("abc"), json!(0), json!(false)] {
        let response = rpc(port, json!({"id":id,"method":"not/a/method"})).await;
        assert_eq!(response.status, 200);
        assert_eq!(
            response.json(),
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found: not/a/method"}})
        );
    }
    let response = rpc(
        port,
        json!({"id":2,"method":"tools/call","params":{"name":"missing"}}),
    )
    .await;
    assert_eq!(
        response.json(),
        json!({"jsonrpc":"2.0","id":2,"error":{"code":-32602,"message":"Unknown tool: missing"}})
    );
    let response = rpc(port, json!({"id":2,"method":"tools/call"})).await;
    assert_eq!(
        response.json()["error"]["message"],
        "Unknown tool: undefined"
    );
    assert!(server.get_log().is_empty());
    for notification in [
        json!({"method":"notifications/initialized"}),
        json!({"id":null,"method":"missing"}),
        json!({}),
        json!(4),
    ] {
        let response = rpc(port, notification).await;
        assert_eq!(response.status, 202);
        assert!(response.body.is_empty());
    }
    for id in [None, Some(Value::Null)] {
        let mut message = json!({"method":"ping"});
        if let Some(id) = id {
            message["id"] = id;
        }
        let response = rpc(port, message.clone()).await;
        assert_eq!(response.status, 200);
        assert_eq!(response.json().get("id"), message.get("id"));
    }
    *fixture.capture.error.lock().unwrap() = Some("capture failed exactly".into());
    let result = tool(port, "take_screenshot", json!({})).await;
    assert_error(&result, "capture failed exactly");
    server.stop().await.unwrap();
}

#[tokio::test]
async fn batches_preserve_order_filter_notifications_and_have_no_session_header() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    for batch in [
        json!([]),
        json!([{"method":"notifications/initialized"},{"id":null,"method":"missing"}]),
    ] {
        let response = rpc(port, batch).await;
        assert_eq!(response.status, 202);
        assert!(response.body.is_empty());
    }
    let response = rpc(
        port,
        json!([
            {"method":"notifications/initialized"},
            {"id":1,"method":"initialize","params":{"protocolVersion":"batch-v"}},
            {"id":2,"method":"tools/call","params":{"name":"list_displays"}},
            {"id":3,"method":"missing"},
            {"id":4,"method":"tools/call","params":{"name":"missing"}},
            {"id":null,"method":"ping"},
        ]),
    )
    .await;
    assert_eq!(response.status, 200);
    assert!(!response.headers.contains_key("mcp-session-id"));
    let answers = response.json();
    let answers = answers.as_array().unwrap();
    assert_eq!(
        answers.iter().map(|a| a["id"].clone()).collect::<Vec<_>>(),
        vec![json!(1), json!(2), json!(3), json!(4), Value::Null]
    );
    assert_eq!(answers[0]["result"]["protocolVersion"], "batch-v");
    assert_eq!(answers[1]["result"]["isError"], false);
    assert_eq!(answers[2]["error"]["code"], -32601);
    assert_eq!(answers[3]["error"]["code"], -32602);
    assert_eq!(answers[4]["result"], json!({}));
    server.stop().await.unwrap();
}

#[tokio::test]
async fn body_limit_is_exactly_64_mib_and_does_not_break_later_requests() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    let mut body = br#"{"id":1,"method":"ping"}"#.to_vec();
    body.resize(BODY_LIMIT, b' ');
    let response = request(port, "POST", "/mcp", &body).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.json()["result"], json!({}));
    body.push(b' ');
    let response = request(port, "POST", "/mcp", &body).await;
    assert_eq!(response.status, 400);
    assert_eq!(response.json()["error"]["code"], -32700);
    assert_eq!(rpc(port, json!({"id":1,"method":"ping"})).await.status, 200);
    server.stop().await.unwrap();
}

#[tokio::test]
async fn test_routes_are_opt_in_and_keep_reference_envelopes() {
    let fixture = Fixture::new();
    let server = fixture
        .server()
        .with_test_handler(Arc::new(|route, body| match route {
            "echo" => Ok(json!({"route":route,"body":body})),
            "nothing" => Ok(Value::Null),
            _ => Err(Error::msg("route handler error")),
        }));
    let port = server.start(0).await.unwrap();
    let response = request(port, "POST", "/test/echo?query=ignored", br#"{"a":2}"#).await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.json(),
        json!({"ok":true,"result":{"route":"echo","body":{"a":2}}})
    );
    for method in ["POST", "GET", "DELETE"] {
        let response = request(port, method, "/test/echo", b"").await;
        assert_eq!(
            response.json(),
            json!({"ok":true,"result":{"route":"echo","body":{}}})
        );
    }
    let response = request(port, "POST", "/test/nothing", b"{}").await;
    assert_eq!(response.json(), json!({"ok":true,"result":null}));
    let response = request(port, "POST", "/test/failure", b"{}").await;
    assert_eq!(response.status, 500);
    assert_eq!(
        response.json(),
        json!({"ok":false,"error":"route handler error"})
    );
    let response = request(port, "POST", "/test/echo", b"{").await;
    assert_eq!(response.status, 500);
    assert_eq!(response.json()["ok"], false);
    assert!(response.json()["error"].is_string());
    server.stop().await.unwrap();
}

#[tokio::test]
async fn call_log_is_newest_first_bounded_and_notified_for_success_and_failure() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let seen = Arc::new(Mutex::new(Vec::<CallLogEntry>::new()));
    let listener_seen = seen.clone();
    server.set_log_listener(Some(Arc::new(move |entry| {
        listener_seen.lock().unwrap().push(entry);
    })));
    let port = server.start(0).await.unwrap();
    for index in 0..61 {
        assert_eq!(
            tool(port, "list_displays", json!({"index":index})).await["isError"],
            false
        );
    }
    assert_error(
        &tool(port, "capture_window", json!({})).await,
        "Pass window_id (from list_windows) or title (substring of the window title)",
    );
    let log = server.get_log();
    assert_eq!(log.len(), 60);
    assert_eq!(log[0].tool, "capture_window");
    assert!(!log[0].ok);
    assert_eq!(
        log[0].error.as_deref(),
        Some("Pass window_id (from list_windows) or title (substring of the window title)")
    );
    assert_eq!(log[1].args, json!({"index":60}));
    assert_eq!(log[59].args, json!({"index":2}));
    assert!(log.iter().all(|e| e.ts > 0));
    assert!(log[1].error.is_none());
    assert!(serde_json::to_value(&log[1])
        .unwrap()
        .get("error")
        .is_none());
    assert_eq!(seen.lock().unwrap().len(), 62);
    assert_eq!(seen.lock().unwrap().last(), Some(&log[0]));
    let mut copy = server.get_log();
    copy.clear();
    assert_eq!(server.get_log().len(), 60);
    server.set_log_listener(Some(Arc::new(|_| panic!("broken listener"))));
    assert_eq!(
        tool(port, "list_displays", json!({})).await["isError"],
        false
    );
    server.stop().await.unwrap();
    assert_eq!(server.get_log().len(), 60); // stop does not reset history
}

#[tokio::test]
async fn slow_ports_and_parallel_batches_leave_http_responsive() {
    let fixture = Fixture::new();
    let blocker = Blocker::new();
    *fixture.capture.blocker.lock().unwrap() = Some(blocker.clone());
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    let batch = tokio::spawn(async move {
        rpc(
            port,
            json!([
                {"id":1,"method":"tools/call","params":{"name":"take_screenshot"}},
                {"id":2,"method":"tools/call","params":{"name":"list_displays"}},
            ]),
        )
        .await
    });
    timeout(Duration::from_secs(5), blocker.entered.notified())
        .await
        .unwrap();
    let ping = timeout(
        Duration::from_secs(2),
        rpc(port, json!({"id":3,"method":"ping"})),
    )
    .await;
    let parallel = timeout(Duration::from_secs(2), async {
        while server.get_log().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    blocker.release(); // release before assertions, including failing assertions
    assert_eq!(ping.unwrap().json()["result"], json!({}));
    parallel.unwrap();
    let result = batch.await.unwrap().json();
    assert_eq!(result[0]["id"], 1);
    assert_eq!(result[1]["id"], 2);
    assert_eq!(result[0]["result"]["isError"], false);
    server.stop().await.unwrap();
}

#[tokio::test]
async fn malformed_batch_member_fails_immediately_while_other_tools_finish() {
    let fixture = Fixture::new();
    let blocker = Blocker::new();
    *fixture.capture.blocker.lock().unwrap() = Some(blocker.clone());
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    let batch = tokio::spawn(async move {
        rpc(
            port,
            json!([
                {"id":1,"method":"tools/call","params":{"name":"take_screenshot"}},
                null,
            ]),
        )
        .await
    });
    timeout(Duration::from_secs(5), blocker.entered.notified())
        .await
        .unwrap();
    let response = timeout(Duration::from_secs(1), batch).await;
    blocker.release();
    let response = response
        .expect("Promise.all rejects before a slower preceding tool finishes")
        .unwrap();
    assert_eq!(response.status, 500);
    assert_eq!(
        response.json(),
        json!({"error":"Cannot destructure property 'method' of 'msg' as it is null."})
    );
    timeout(Duration::from_secs(5), async {
        while server.get_log().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(server.get_log()[0].ok);
    server.stop().await.unwrap();
}
