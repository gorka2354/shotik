mod common;

use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::*;
use serde_json::{json, Value};
use shotik_core::ports::{HistoryStore, MainEvent, RegionSelection};
use shotik_core::{Frame, PointF};
use tokio::time::timeout;

#[tokio::test]
async fn tool_names_descriptions_and_schemas_match_js_verbatim() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut command = Command::new("node");
    command
        .arg(root.join("tests/reference-tools.cjs"))
        .arg(root.join("../../src/main/main.js"));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let reference = command
        .output()
        .expect("Node is required to compare the JS reference");
    assert!(
        reference.status.success(),
        "{}",
        String::from_utf8_lossy(&reference.stderr)
    );
    let reference: Value = serde_json::from_slice(&reference.stdout).unwrap();
    let fixture = Fixture::new();
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    let response = rpc(port, json!({"id":1,"method":"tools/list"})).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.json()["result"]["tools"], reference);
    assert_eq!(reference.as_array().unwrap().len(), 10);
    server.stop().await.unwrap();
}

#[tokio::test]
async fn list_displays_matches_dip_shape_and_literal_json_number_format() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    let result = tool(port, "list_displays", json!({})).await;
    assert_eq!(result["isError"], false);
    assert_eq!(
        parsed_text(&result),
        json!([
            {"id":1,"bounds":{"x":0,"y":0,"width":160,"height":120},"scaleFactor":1.5,"primary":true},
            {"id":2,"bounds":{"x":160,"y":20,"width":200,"height":100},"scaleFactor":1,"primary":false},
            {"id":3,"bounds":{"x":-100,"y":-20,"width":100,"height":100},"scaleFactor":1,"primary":false},
        ])
    );
    assert!(text(&result).contains("\"scaleFactor\": 1,"));
    assert!(!text(&result).contains("1.0"));
    {
        let events = fixture.ui.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], MainEvent::McpLog(v) if v["tool"] == "list_displays"));
    }
    server.stop().await.unwrap();
}

#[tokio::test]
async fn screenshots_follow_cursor_or_explicit_display_and_always_save() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    let result = tool(port, "take_screenshot", json!({})).await;
    assert_eq!(
        (image(&result).width(), image(&result).height()),
        (200, 100)
    );
    let entry = fixture.history.items.lock().unwrap()[0].clone();
    assert!(entry.file.exists());
    assert_eq!(entry.source, "mcp");
    assert_eq!(
        text(&result),
        format!(
            "Captured display 2 (secondary, bounds 200x100 at 160,20). Saved to {} (200x100px)",
            entry.file.display()
        )
    );
    assert!(!fixture.settings.0.lock().unwrap().auto_save);
    *fixture.displays.cursor.lock().unwrap() = PointF { x: -50.0, y: 10.0 };
    assert!(text(&tool(port, "take_screenshot", json!({})).await).starts_with("Captured display 3"));
    for id in [json!(1), json!(999), json!("2")] {
        assert!(
            text(&tool(port, "take_screenshot", json!({"display_id":id})).await)
                .starts_with("Captured display 1")
        );
    }
    assert!(
        text(&tool(port, "take_screenshot", json!({"display_id":0})).await)
            .starts_with("Captured display 3")
    );
    let changed = fixture
        .ui
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| matches!(event, MainEvent::HistoryChanged))
        .count();
    assert_eq!(changed, 6);
    *fixture.capture.error.lock().unwrap() = Some("No screen sources available".into());
    assert_error(
        &tool(port, "take_screenshot", json!({})).await,
        "No screen sources available",
    );
    *fixture.capture.error.lock().unwrap() = None;
    *fixture.history.save_error.lock().unwrap() = Some("cannot save screenshot".into());
    assert_error(
        &tool(port, "take_screenshot", json!({})).await,
        "cannot save screenshot",
    );
    *fixture.capture.override_frame.lock().unwrap() = Some(Frame::new(0, 0, vec![]).unwrap());
    assert_error(
        &tool(port, "take_screenshot", json!({})).await,
        "Capture produced an empty image",
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn image_downscaling_and_full_resolution_apply_to_capture_and_history() {
    let fixture = Fixture::new();
    *fixture.capture.override_frame.lock().unwrap() = Some(patterned(3200, 400));
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    for (arguments, expected) in [
        (json!({}), (1600, 200)),
        (json!({"full_resolution":true}), (3200, 400)),
    ] {
        let captured = tool(port, "take_screenshot", arguments.clone()).await;
        let frame = image(&captured);
        assert_eq!((frame.width(), frame.height()), expected);
        assert!(text(&captured).ends_with("(3200x400px)"));
        let last = tool(port, "get_last_screenshot", arguments).await;
        let frame = image(&last);
        assert_eq!((frame.width(), frame.height()), expected);
    }
    fixture.settings.0.lock().unwrap().downscale_for_ai = false;
    assert_eq!(
        image(&tool(port, "take_screenshot", json!({})).await).width(),
        3200
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn regions_use_global_dip_clip_and_scale_without_history_side_effects() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    for (args, dimensions, expected_text) in [
        (json!({"x":180,"y":40,"width":20,"height":30}), (20,30), "Captured 20x30 DIP at global (180, 40) (display 2)"),
        (json!({"x":0,"y":0,"width":20,"height":10}), (30,15), "Captured 20x10 DIP at global (0, 0) (display 1)"),
        (json!({"x":-90,"y":-10,"width":12,"height":11}), (12,11), "Captured 12x11 DIP at global (-90, -10) (display 3)"),
        (json!({"x":130,"y":40,"width":100,"height":20}), (70,20), "Captured 70x20 DIP at global (160, 40) (clipped to display 2: requested region crossed its edge)"),
        (json!({"x":130,"y":40,"width":100,"height":20,"display_id":"1"}), (45,30), "Captured 30x20 DIP at global (130, 40) (clipped to display 1: requested region crossed its edge)"),
    ] {
        let result = tool(port,"take_screenshot_region",args).await;
        let frame = image(&result);
        assert_eq!((frame.width(),frame.height()), dimensions);
        assert_eq!(text(&result), expected_text);
    }
    assert!(fixture.history.items.lock().unwrap().is_empty());
    assert!(fixture
        .ui
        .events
        .lock()
        .unwrap()
        .iter()
        .all(|event| !matches!(event, MainEvent::HistoryChanged)));
    assert_error(&tool(port,"take_screenshot_region",json!({})).await,
        "Invalid region: \"x\" must be a finite number, got undefined. Pass x, y, width, height in global virtual-desktop DIP coordinates (same space as list_displays bounds).");
    *fixture.capture.error.lock().unwrap() = Some("grab failed".into());
    assert_error(
        &tool(
            port,
            "take_screenshot_region",
            json!({"x":0,"y":0,"width":10,"height":10}),
        )
        .await,
        "grab failed",
    );
    *fixture.capture.error.lock().unwrap() = None;
    *fixture.capture.override_frame.lock().unwrap() = Some(patterned(1, 1));
    assert_error(
        &tool(
            port,
            "take_screenshot_region",
            json!({"x":100,"y":100,"width":10,"height":10}),
        )
        .await,
        "Region outside display bounds",
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn selection_returns_image_cancellation_timeout_and_typed_errors() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    let selected = tool(port, "ask_user_to_select_region", json!({})).await;
    assert_eq!(selected["isError"], false);
    assert_eq!(selected["content"].as_array().unwrap().len(), 1);
    assert_eq!(
        (image(&selected).width(), image(&selected).height()),
        (12, 10)
    );
    assert_eq!(*fixture.timeouts.lock().unwrap(), vec![120_000]);
    for (selection, expected) in [
        (Selection::Cancel, "The user cancelled the selection."),
        (
            Selection::Timeout,
            "Timed out waiting for the user (2 min).",
        ),
    ] {
        *fixture.selection.lock().unwrap() = selection;
        let result = tool(port, "ask_user_to_select_region", json!({})).await;
        assert_eq!(
            result,
            json!({"content":[{"type":"text","text":expected}],"isError":false})
        );
    }
    *fixture.selection.lock().unwrap() = Selection::Error("selection failed".into());
    assert_error(
        &tool(port, "ask_user_to_select_region", json!({})).await,
        "selection failed",
    );
    fixture.ui.overlay.store(true, Ordering::SeqCst);
    assert_error(
        &tool(port, "ask_user_to_select_region", json!({})).await,
        "Selection overlay is already open",
    );
    fixture.ui.overlay.store(false, Ordering::SeqCst);
    for (png, expected) in [
        (vec![], "Capture produced an empty image"),
        (
            b"not an image".to_vec(),
            "Capture produced an unreadable image",
        ),
    ] {
        *fixture.selection.lock().unwrap() = Selection::Png(png);
        assert_error(
            &tool(port, "ask_user_to_select_region", json!({})).await,
            expected,
        );
    }
    server.stop().await.unwrap();
}

#[tokio::test]
async fn waiting_for_selection_does_not_block_other_requests() {
    let fixture = Fixture::new();
    *fixture.selection.lock().unwrap() = Selection::Deferred;
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    let waiting =
        tokio::spawn(async move { tool(port, "ask_user_to_select_region", json!({})).await });
    timeout(Duration::from_secs(5), fixture.selection_entered.notified())
        .await
        .unwrap();
    let ping = timeout(
        Duration::from_secs(2),
        rpc(port, json!({"id":1,"method":"ping"})),
    )
    .await;
    fixture.pending_selection.lock().unwrap().take().unwrap()(Ok(RegionSelection::Cancelled));
    assert_eq!(ping.unwrap().status, 200);
    assert_eq!(
        text(&waiting.await.unwrap()),
        "The user cancelled the selection."
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn selection_deadline_is_enforced_without_an_application_callback() {
    let fixture = Fixture::new();
    *fixture.selection.lock().unwrap() = Selection::Deferred;
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    let waiting =
        tokio::spawn(async move { tool(port, "ask_user_to_select_region", json!({})).await });
    timeout(Duration::from_secs(5), fixture.selection_entered.notified())
        .await
        .unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(119)).await;
    assert!(
        !waiting.is_finished(),
        "the two-minute deadline must not fire early"
    );
    tokio::time::advance(Duration::from_secs(2)).await;
    let result = waiting.await.unwrap();
    tokio::time::resume();
    assert_eq!(text(&result), "Timed out waiting for the user (2 min).");
    assert_eq!(result["isError"], false);
    // A late callback is ignored, as with Promise.race in the reference.
    fixture.pending_selection.lock().unwrap().take().unwrap()(Ok(RegionSelection::Cancelled));
    assert_eq!(server.get_log().len(), 1);
    server.stop().await.unwrap();
}

#[tokio::test]
async fn screenshots_filter_out_recordings_and_report_unreadable_images_and_invalid_dates() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    assert_eq!(
        text(&tool(port, "get_last_screenshot", json!({})).await),
        "No screenshots in history yet."
    );
    assert_eq!(text(&tool(port, "list_screenshots", json!({})).await), "[]");
    let frame = patterned(20, 10);
    let shot = fixture
        .history
        .save_png(&frame.to_png().unwrap(), &frame, "region", None)
        .unwrap();
    fixture.history.items.lock().unwrap()[0].kind = None; // pre-recording history
    fixture
        .history
        .entry("video.mp4", Some("video"), 1_700_000_001_123, Some(2.0));
    fixture
        .history
        .entry("audio.m4a", Some("audio"), 1_700_000_002_123, Some(3.0));
    let last = tool(port, "get_last_screenshot", json!({})).await;
    assert_eq!(image(&last).width(), 20);
    assert_eq!(
        text(&last),
        format!(
            "{} (20x10px, taken 2023-11-14T22:13:20.123Z)",
            shot.file.display()
        )
    );
    let shots = tool(port, "list_screenshots", json!({"limit":50})).await;
    assert_eq!(text(&shots), serde_json::to_string_pretty(&json!([
        {"file":shot.file,"width":20,"height":10,"taken":"2023-11-14T22:13:20.123Z","source":"region"}
    ])).unwrap());
    std::fs::write(&shot.file, b"").unwrap();
    assert_error(
        &tool(port, "get_last_screenshot", json!({})).await,
        "Capture produced an empty image",
    );
    std::fs::write(&shot.file, b"not a PNG").unwrap();
    assert_error(
        &tool(port, "get_last_screenshot", json!({})).await,
        "Capture produced an unreadable image",
    );
    std::fs::write(&shot.file, frame.to_png().unwrap()).unwrap();
    fixture
        .history
        .items
        .lock()
        .unwrap()
        .iter_mut()
        .find(|it| it.id == shot.id)
        .unwrap()
        .ts = i64::MAX;
    assert_error(
        &tool(port, "get_last_screenshot", json!({})).await,
        "Invalid time value",
    );
    assert_error(
        &tool(port, "list_screenshots", json!({})).await,
        "Invalid time value",
    );
    std::fs::remove_file(&shot.file).unwrap();
    assert_eq!(
        text(&tool(port, "get_last_screenshot", json!({})).await),
        "No screenshots in history yet."
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn list_screenshot_limit_preserves_reference_slice_semantics() {
    let fixture = Fixture::new();
    for i in 0..55 {
        fixture
            .history
            .entry(&format!("image-{i}.png"), None, 0, None);
    }
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    for (args, count) in [
        (json!({}), 10),
        (json!({"limit":0}), 10),
        (json!({"limit":2.8}), 2),
        (json!({"limit":100}), 50),
        (json!({"limit":-3}), 52),
        (json!({"limit":"junk"}), 0),
    ] {
        let result = parsed_text(&tool(port, "list_screenshots", args).await);
        assert_eq!(result.as_array().unwrap().len(), count);
    }
    server.stop().await.unwrap();
}
