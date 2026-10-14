use crate::app_event::AppEvent;
use crate::bottom_pane::StatusLineItem;
use crate::chatwidget::ReplayKind;
use crate::chatwidget::tests::make_chatwidget_manual;
use crate::chatwidget::tests::render_bottom_popup;
use crate::codex_plus_plus::prompt_cache::CacheObservation;
use crate::codex_plus_plus::prompt_cache::PromptCache;
use codex_app_server_protocol::ServerNotification;
use codex_protocol::ThreadId;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;

fn completed(thread: ThreadId, id: &str, input: i64, cached: i64) -> ServerNotification {
    serde_json::from_value(json!({
        "method": "rawResponse/completed", "params": {
            "threadId": thread.to_string(), "turnId": "turn", "responseId": id,
            "usage": {"totalTokens": input, "inputTokens": input,
                "cachedInputTokens": cached, "outputTokens": 0, "reasoningOutputTokens": 0},
            "usageMetadata": {"amount": null, "metadata": {
                "input_tokens": input, "input_tokens_details": {"cached_tokens": cached}
            }}
        }
    }))
    .unwrap()
}

#[tokio::test]
async fn prompt_cache_latest_footer_holds_misses_and_preserves_session_average() {
    let (mut chat, mut events, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    tokio::time::pause();
    let thread = ThreadId::new();
    chat.thread_id = Some(thread);
    chat.local_settings.tui.status_line = Some(vec!["prompt-cache".into()]);
    chat.local_settings.tui.status_line_use_colors = false;
    let (frame_requester, mut frames) = crate::tui::FrameRequester::test_channel();
    chat.frame_requester = frame_requester;
    chat.refresh_status_line();
    let mut rendered = Vec::new();
    rendered.push(format!(
        "unknown:\n{}",
        render_bottom_popup(&chat, /*width*/ 100)
    ));
    chat.add_status_output(
        /*refreshing_rate_limits*/ false, /*request_id*/ None,
    );
    while let Ok(event) = events.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            rendered.extend(
                cell.display_lines(/*width*/ 100)
                    .iter()
                    .map(ToString::to_string)
                    .filter(|line| line.contains("Cache:")),
            );
        }
    }
    let cold = completed(thread, "cold", /*input*/ 100, /*cached*/ 0);
    let hit = completed(thread, "hit", /*input*/ 900, /*cached*/ 800);
    chat.handle_server_notification(cold.clone(), /*replay_kind*/ None);
    rendered.push(format!(
        "cold:\n{}",
        render_bottom_popup(&chat, /*width*/ 100)
    ));
    // The initial immediate draw coalesces delayed draws; each frame must re-arm expiry.
    while frames.try_recv().is_ok() {}
    chat.pre_draw_tick();
    let minimum_deadline = std::time::Instant::now() + Duration::from_secs(/*secs*/ 2);
    assert!(
        std::iter::from_fn(|| frames.try_recv().ok()).any(|deadline| deadline > minimum_deadline)
    );
    tokio::time::advance(Duration::from_secs(/*secs*/ 1)).await;
    chat.handle_server_notification(hit.clone(), /*replay_kind*/ None);
    assert_eq!(chat.prompt_cache.status_line().0, "Cache 0.0% (miss)");
    tokio::time::advance(Duration::from_secs(/*secs*/ 2)).await;
    chat.pre_draw_tick();
    rendered.push(format!(
        "hit:\n{}",
        render_bottom_popup(&chat, /*width*/ 100)
    ));
    assert_eq!(
        chat.status_line_value_for_item(StatusLineItem::PromptCache),
        Some("Cache 88.9%".into())
    );
    assert_eq!(
        chat.status_surface_preview_data()
            .status_line_for_items(
                [StatusLineItem::PromptCache],
                /*use_theme_colors*/ false
            )
            .unwrap()
            .to_string(),
        "Cache 88.9%"
    );
    for notification in [cold, hit] {
        chat.handle_server_notification(notification, /*replay_kind*/ None);
    }
    for replay in [
        ReplayKind::ResumeInitialMessages,
        ReplayKind::ThreadSnapshot,
    ] {
        chat.handle_server_notification(
            completed(thread, "historical", /*input*/ 5000, /*cached*/ 0),
            Some(replay),
        );
    }
    chat.handle_server_notification(
        completed(thread, "historical", /*input*/ 5000, /*cached*/ 0),
        /*replay_kind*/ None,
    );
    chat.handle_server_notification(
        completed(
            ThreadId::new(),
            "foreign",
            /*input*/ 100,
            /*cached*/ 0,
        ),
        /*replay_kind*/ None,
    );
    assert_eq!(
        chat.prompt_cache.status_line(),
        ("Cache 88.9%".into(), None)
    );
    chat.handle_server_notification(
        completed(thread, "empty", /*input*/ 0, /*cached*/ 0),
        /*replay_kind*/ None,
    );
    assert_eq!(
        chat.prompt_cache.status_line(),
        ("Cache unknown".into(), None)
    );
    chat.add_status_output(
        /*refreshing_rate_limits*/ false, /*request_id*/ None,
    );
    while let Ok(event) = events.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            rendered.extend(
                cell.display_lines(/*width*/ 100)
                    .iter()
                    .map(ToString::to_string)
                    .filter(|line| line.contains("Cache:")),
            );
        }
    }
    assert_eq!(
        chat.prompt_cache.status(),
        "80.0% of reported input this session"
    );
    insta::assert_snapshot!(rendered.join("\n"));
}

#[test]
fn prompt_cache_missing_usage_replaces_latest_but_preserves_reported_average() {
    let thread = ThreadId::new();
    for raw in [
        json!(null),
        json!({}),
        json!({"input_tokens": 100}),
        json!({"input_tokens": 100, "input_tokens_details": {}}),
        json!({"input_tokens": 100, "input_tokens_details": {"cached_tokens": null}}),
    ] {
        let cache = PromptCache::default();
        cache.observe(
            &completed(thread, "hit", /*input*/ 100, /*cached*/ 75),
            /*active*/ true,
        );
        let ServerNotification::RawResponseCompleted(mut completion) =
            completed(thread, "response", /*input*/ 100, /*cached*/ 0)
        else {
            unreachable!()
        };
        completion.usage_metadata.as_mut().unwrap().metadata = Some(raw);
        cache.observe_completion(&completion, CacheObservation::Active);
        assert_eq!(
            (cache.status(), cache.status_line()),
            (
                "75.0% of reported input this session".into(),
                ("Cache unknown".into(), None)
            )
        );
    }
}
