mod common;

use common::*;
use serde_json::json;
use shotik_core::ports::{NativeWindow, WindowCaptureMethod};
use shotik_core::{Frame, Rect};
use shotik_mcp::window_capture::{looks_blank, score_title};
use shotik_mcp::{McpServer, WindowCaptureOptions};

#[tokio::test]
async fn list_windows_reports_global_dip_null_bounds_for_minimized_and_errors() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    let result = tool(port, "list_windows", json!({})).await;
    assert_eq!(result["isError"], false);
    assert_eq!(
        parsed_text(&result),
        json!([
            {"id":42,"title":"Editor","app":"edit.exe","bounds":{"x":20,"y":30,"width":80,"height":60},"display_id":1,"minimized":false},
            {"id":43,"title":"Editor notes","app":null,"bounds":{"x":170,"y":30,"width":80,"height":60},"display_id":2,"minimized":false},
            {"id":44,"title":"Minimized","app":null,"bounds":null,"display_id":null,"minimized":true},
        ])
    );
    assert_eq!(*fixture.windows.excluded.lock().unwrap(), vec![None]);
    *fixture.windows.list_error.lock().unwrap() =
        Some("window enumeration returned malformed data".into());
    assert_error(
        &tool(port, "list_windows", json!({})).await,
        "window enumeration returned malformed data",
    );
    *fixture.windows.list_error.lock().unwrap() = None;
    fixture.windows.items.lock().unwrap().clear();
    assert_eq!(text(&tool(port, "list_windows", json!({})).await), "[]");
    server.stop().await.unwrap();

    let normal_server = McpServer::new("version", fixture.services());
    let port = normal_server.start(0).await.unwrap();
    tool(port, "list_windows", json!({})).await;
    assert_eq!(
        fixture.windows.excluded.lock().unwrap().last(),
        Some(&Some(std::process::id()))
    );
    normal_server.stop().await.unwrap();
}

#[tokio::test]
async fn mixed_dpi_bounds_keep_display_offsets_and_negative_origins() {
    let fixture = Fixture::new();
    *fixture.displays.items.lock().unwrap() = vec![
        display(
            1,
            Rect::new(0, 0, 2560, 1440),
            1.5,
            Rect::new(0, 0, 3840, 2160),
            true,
        ),
        display(
            2,
            Rect::new(2560, 462, 1920, 1080),
            1.0,
            Rect::new(3840, 694, 1920, 1080),
            false,
        ),
        display(
            3,
            Rect::new(-2560, 279, 2560, 1440),
            1.0,
            Rect::new(-2560, 419, 2560, 1440),
            false,
        ),
    ];
    *fixture.windows.items.lock().unwrap() = vec![
        NativeWindow {
            id: 1,
            title: "primary".into(),
            app: None,
            bounds: Some(Rect::new(150, 300, 900, 600)),
            minimized: false,
        },
        NativeWindow {
            id: 2,
            title: "secondary".into(),
            app: None,
            bounds: Some(Rect::new(3900, 724, 900, 600)),
            minimized: false,
        },
        NativeWindow {
            id: 3,
            title: "left".into(),
            app: None,
            bounds: Some(Rect::new(-2500, 450, 900, 600)),
            minimized: false,
        },
    ];
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    let result = parsed_text(&tool(port, "list_windows", json!({})).await);
    for (index, bounds) in [
        Rect::new(100, 200, 600, 400),
        Rect::new(2620, 492, 900, 600),
        Rect::new(-2500, 310, 900, 600),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(result[index]["bounds"], json!(bounds));
        assert_eq!(result[index]["display_id"], json!(index + 1));
    }
    server.stop().await.unwrap();
}

#[tokio::test]
async fn capture_window_matches_title_id_and_staleness_guard_with_exact_replies() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    for args in [
        json!({"window_id":42}),
        json!({"window_id":"42","title":"EDITOR"}),
        json!({"title":"editor"}),
    ] {
        let result = tool(port, "capture_window", args).await;
        assert_eq!(result["isError"], false);
        let entry = fixture.history.items.lock().unwrap()[0].clone();
        let extra = if fixture.history.items.lock().unwrap().len() == 3 {
            " Other windows matching the title: \"Editor notes\" — pass window_id to disambiguate."
        } else {
            ""
        };
        assert_eq!(text(&result),format!("Captured window \"Editor\" (edit.exe) via printwindow. Saved to {} (120x90px).{extra}",entry.file.display()));
        assert_eq!((image(&result).width(), image(&result).height()), (120, 90));
    }
    assert_eq!(
        *fixture.windows.calls.lock().unwrap(),
        vec![(42, WindowCaptureMethod::PrintWindow); 3]
    );
    for (args,message) in [
        (json!({}),"Pass window_id (from list_windows) or title (substring of the window title)"),
        (json!({"window_id":999}),"No window with id 999. Use list_windows for current ids."),
        (json!({"title":"absent"}),"No window matches title \"absent\". Use list_windows for exact titles."),
        (json!({"window_id":42,"title":"another app"}),"Window id 42 now belongs to \"Editor\", which does not match title \"another app\" — the id is stale (window ids get recycled). Call list_windows again for a fresh id."),
        (json!({"window_id":44}),"Window \"Minimized\" is minimized — its surface has no pixels while iconic. Ask the user to restore it, then retry."),
        (json!({"window_id":42,"title":true}),"query.toLowerCase is not a function"),
        (json!({"title":123}),"query.toLowerCase is not a function"),
    ] {
        assert_error(&tool(port,"capture_window",args).await,message);
    }
    fixture.windows.items.lock().unwrap().clear();
    assert_error(
        &tool(port, "capture_window", json!({"title":"editor"})).await,
        "Could not enumerate windows",
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn title_scores_are_exact_then_prefix_then_substring_with_stable_ties_and_five_other_matches()
{
    assert_eq!(score_title("ЕДИТОР", "едитор"), 3);
    assert_eq!(score_title("Editor notes", "EDITOR"), 2);
    assert_eq!(score_title("My editor", "EDITOR"), 1);
    assert_eq!(score_title("Browser", "editor"), 0);
    let fixture = Fixture::new();
    let names = [
        "My edit",
        "Edit prefix",
        "EDIT",
        "Edit second",
        "A edit",
        "B edit",
        "C edit",
        "D edit",
    ];
    *fixture.windows.items.lock().unwrap() = names
        .iter()
        .enumerate()
        .map(|(i, title)| NativeWindow {
            id: i as u64 + 1,
            title: (*title).into(),
            app: None,
            bounds: Some(Rect::new(0, 0, 100, 100)),
            minimized: false,
        })
        .collect();
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    let result = tool(port, "capture_window", json!({"title":"edit"})).await;
    assert!(text(&result).starts_with("Captured window \"EDIT\" via printwindow."));
    assert!(text(&result).ends_with("Other windows matching the title: \"Edit prefix\", \"Edit second\", \"My edit\", \"A edit\", \"B edit\" — pass window_id to disambiguate."));
    assert_eq!(fixture.windows.calls.lock().unwrap()[0].0, 3);
    server.stop().await.unwrap();
}

#[tokio::test]
async fn window_fallback_is_only_used_after_blank_or_failed_printwindow() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    for (primary, fallback, method, uniform, calls) in [
        (
            Ok(patterned(32, 20)),
            Ok(patterned(50, 40)),
            "printwindow",
            false,
            1,
        ),
        (
            Err("print failed".into()),
            Ok(patterned(50, 40)),
            "graphicscapture",
            false,
            2,
        ),
        (
            Ok(flat(32, 20)),
            Ok(patterned(50, 40)),
            "graphicscapture",
            false,
            2,
        ),
        (Ok(flat(32, 20)), Ok(flat(50, 40)), "printwindow", true, 2),
        (
            Ok(flat(32, 20)),
            Err("fallback failed".into()),
            "printwindow",
            true,
            2,
        ),
        (
            Err("print failed".into()),
            Ok(flat(50, 40)),
            "graphicscapture",
            true,
            2,
        ),
        (
            Ok(flat(32, 20)),
            Ok(Frame::new(0, 0, vec![]).unwrap()),
            "printwindow",
            true,
            2,
        ),
        (
            Ok(Frame::new(0, 0, vec![]).unwrap()),
            Ok(patterned(50, 40)),
            "graphicscapture",
            false,
            2,
        ),
    ] {
        *fixture.windows.primary.lock().unwrap() = primary;
        *fixture.windows.fallback.lock().unwrap() = fallback;
        fixture.windows.calls.lock().unwrap().clear();
        let result = tool(port, "capture_window", json!({"window_id":42})).await;
        assert_eq!(result["isError"], false, "{result}");
        assert!(
            text(&result).contains(&format!("via {method}.")),
            "{result}"
        );
        assert_eq!(
            text(&result).contains("Note: the image is one flat color"),
            uniform
        );
        assert_eq!(fixture.windows.calls.lock().unwrap().len(), calls);
        let expected_width = if method == "printwindow" { 32 } else { 50 };
        assert_eq!(image(&result).width(), expected_width);
    }
    *fixture.windows.primary.lock().unwrap() = Err("print failed".into());
    *fixture.windows.fallback.lock().unwrap() = Err("fallback failed".into());
    assert_error(&tool(port,"capture_window",json!({"window_id":42})).await,
        "Could not capture \"Editor\" (print failed). The window may use a GPU-only surface; take_screenshot of its display is the fallback.");
    *fixture.windows.primary.lock().unwrap() = Ok(Frame::new(0, 0, vec![]).unwrap());
    assert_error(&tool(port,"capture_window",json!({"window_id":42})).await,
        "Could not capture \"Editor\" (PrintWindow produced an empty image). The window may use a GPU-only surface; take_screenshot of its display is the fallback.");
    server.stop().await.unwrap();
}

#[tokio::test]
async fn forced_window_capture_failure_is_supplied_as_parameter() {
    let fixture = Fixture::new();
    *fixture.windows.fallback.lock().unwrap() = Ok(patterned(80, 60));
    let server = McpServer::new("version", fixture.services()).with_window_capture_options(
        WindowCaptureOptions {
            exclude_pid: None,
            force_print_window_failure: true,
        },
    );
    let port = server.start(0).await.unwrap();
    let result = tool(port, "capture_window", json!({"window_id":42})).await;
    assert!(text(&result).contains("via graphicscapture."));
    assert_eq!(
        *fixture.windows.calls.lock().unwrap(),
        vec![(42, WindowCaptureMethod::GraphicsCapture)]
    );
    *fixture.windows.fallback.lock().unwrap() = Err("fallback failed".into());
    assert_error(&tool(port,"capture_window",json!({"window_id":42})).await,
        "Could not capture \"Editor\" (forced failure (test)). The window may use a GPU-only surface; take_screenshot of its display is the fallback.");
    server.stop().await.unwrap();
}

#[test]
fn looks_blank_samples_reference_grid_with_three_channel_tolerance() {
    assert!(looks_blank(&Frame::new(0, 0, vec![]).unwrap()));
    assert!(looks_blank(&patterned(1, 20)));
    assert!(looks_blank(&flat(64, 64)));
    let mut pixels = flat(64, 64).bgra().to_vec();
    pixels[4] += 30; // x=1 is not on the 16x16 sample grid
    pixels[16] += 3; // x=4, tolerance includes exactly 3
    pixels[19] += 100; // alpha does not influence looksBlank
    assert!(looks_blank(&Frame::new(64, 64, pixels.clone()).unwrap()));
    pixels[16] += 1;
    assert!(!looks_blank(&Frame::new(64, 64, pixels).unwrap()));
    assert!(!looks_blank(&patterned(32, 20)));
}
