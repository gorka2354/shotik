mod common;

use common::*;
use serde_json::{json, Value};

#[tokio::test]
async fn recordings_filter_before_limit_and_return_verbatim_json_shapes() {
    let fixture = Fixture::new();
    fixture.history.entry("old.png", None, 0, None);
    let video = fixture
        .history
        .entry("screen.mp4", Some("video"), 1_700_000_000_123, Some(3.5));
    let audio = fixture
        .history
        .entry("call.m4a", Some("audio"), 1_700_000_001_123, None);
    std::fs::File::create(&video.file)
        .unwrap()
        .set_len(1_500_000)
        .unwrap();
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    let expected_audio = json!({"file":audio.file,"kind":"audio","duration_sec":0,"size_mb":0,"recorded":"2023-11-14T22:13:21.123Z"});
    let expected_video = json!({"file":video.file,"kind":"video","duration_sec":3.5,"size_mb":1.4,"recorded":"2023-11-14T22:13:20.123Z","width":320,"height":180});
    let recent = tool(port, "list_recordings", json!({"limit":2})).await;
    assert_eq!(
        text(&recent),
        serde_json::to_string_pretty(&json!([expected_audio, expected_video])).unwrap()
    );
    for (args, expected) in [
        (json!({}), expected_audio.clone()),
        (json!({"kind":"audio"}), expected_audio.clone()),
        (json!({"kind":"video"}), expected_video.clone()),
    ] {
        let last = tool(port, "get_last_recording", args).await;
        assert_eq!(last["isError"], false);
        assert_eq!(
            text(&last),
            serde_json::to_string_pretty(&expected).unwrap()
        );
        assert_eq!(last["content"].as_array().unwrap().len(), 1);
    }
    for (kind, expected) in [("audio", expected_audio), ("video", expected_video)] {
        let list = tool(port, "list_recordings", json!({"kind":kind,"limit":1})).await;
        assert_eq!(parsed_text(&list), json!([expected]));
    }
    std::fs::remove_file(audio.file).unwrap();
    assert_eq!(
        parsed_text(&tool(port, "get_last_recording", json!({})).await)["file"],
        json!(video.file)
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn recording_limits_kind_validation_and_empty_messages() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    for (args, expected) in [
        (json!({}), "No recordings in history yet."),
        (
            json!({"kind":"audio"}),
            "No audio recordings in history yet.",
        ),
        (
            json!({"kind":"video"}),
            "No video recordings in history yet.",
        ),
    ] {
        assert_eq!(
            text(&tool(port, "get_last_recording", args.clone()).await),
            expected
        );
        assert_eq!(text(&tool(port, "list_recordings", args).await), "[]");
    }
    for kind in [json!("mp3"), json!(null), json!(false), json!(1), json!("")] {
        for name in ["get_last_recording", "list_recordings"] {
            assert_error(
                &tool(port, name, json!({"kind":kind})).await,
                &format!("\"kind\" must be \"audio\" or \"video\", got {kind}"),
            );
        }
    }
    for i in 0..55 {
        fixture
            .history
            .entry(&format!("call-{i}.m4a"), Some("audio"), 0, Some(0.0));
    }
    for (args, count) in [
        (json!({}), 10),
        (json!({"limit":null}), 10),
        (json!({"limit":0}), 10),
        (json!({"limit":-3}), 10),
        (json!({"limit":"junk"}), 10),
        (json!({"limit":0.9}), 10),
        (json!({"limit":"2.9"}), 2),
        (json!({"limit":100}), 50),
    ] {
        let result = tool(port, "list_recordings", args).await;
        assert_eq!(parsed_text(&result).as_array().unwrap().len(), count);
    }
    assert_eq!(
        parsed_text(&tool(port, "get_last_recording", json!({"limit":50})).await)["file"],
        json!(fixture.dir.0.join("call-54.m4a"))
    );
    fixture.history.items.lock().unwrap()[0].ts = i64::MAX;
    for name in ["get_last_recording", "list_recordings"] {
        assert_error(&tool(port, name, json!({})).await, "Invalid time value");
    }
    server.stop().await.unwrap();
}

#[tokio::test]
async fn in_progress_note_is_appended_for_both_tools_even_when_no_finished_match() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let port = server.start(0).await.unwrap();
    for (audio_only, what) in [
        (Some(true), "An audio recording"),
        (Some(false), "A screen recording"),
        (None, "A screen recording"),
    ] {
        {
            let mut status = fixture.recorder.0.lock().unwrap();
            status.recording = true;
            status.audio_only = audio_only;
        }
        for name in ["get_last_recording", "list_recordings"] {
            // The note describes the real session, even if a different kind was requested.
            let result = tool(port, name, json!({"kind":"audio"})).await;
            assert_eq!(result["isError"], false);
            assert_eq!(result["content"].as_array().unwrap().len(), 2);
            assert_eq!(
                result["content"][1],
                json!({"type":"text","text":format!("{what} is in progress right now; it appears here once stopped.")})
            );
        }
    }
    fixture
        .history
        .entry("finished.m4a", Some("audio"), 0, Some(2.0));
    let result = tool(port, "get_last_recording", json!({})).await;
    assert_eq!(parsed_text(&result)["kind"], "audio");
    assert_eq!(result["content"].as_array().unwrap().len(), 2);
    fixture.recorder.0.lock().unwrap().recording = false;
    let result = tool(port, "list_recordings", json!({})).await;
    assert_eq!(result["content"].as_array().unwrap().len(), 1);
    assert!(result["content"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["type"] == Value::String("text".into())));
    server.stop().await.unwrap();
}
