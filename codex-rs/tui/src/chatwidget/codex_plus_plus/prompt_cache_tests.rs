use crate::app_event::AppEvent;
use crate::chatwidget::ReplayKind;
use crate::chatwidget::tests::make_chatwidget_manual;
use crate::codex_plus_plus::prompt_cache::PromptCache;
use codex_app_server_protocol::ServerNotification;
use codex_protocol::ThreadId;
use pretty_assertions::assert_eq;
use serde_json::json;

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
async fn prompt_cache_reports_only_live_misses_and_token_weighted_status() {
    let (mut chat, mut events, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread = ThreadId::new();
    chat.thread_id = Some(thread);
    let mut rendered = Vec::new();
    chat.add_status_output(
        /*refreshing_rate_limits*/ false, /*request_id*/ None,
    );
    while let Ok(event) = events.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            rendered.extend(
                cell.display_lines(100)
                    .iter()
                    .map(ToString::to_string)
                    .filter(|line| line.contains("Cache:")),
            );
        }
    }
    let cold = completed(thread, "cold", 100, 0);
    let hit = completed(thread, "hit", 900, 800);
    for notification in [cold.clone(), hit.clone(), cold, hit] {
        chat.handle_server_notification(notification, /*replay_kind*/ None);
    }
    for replay in [
        ReplayKind::ResumeInitialMessages,
        ReplayKind::ThreadSnapshot,
    ] {
        chat.handle_server_notification(completed(thread, "historical", 5000, 0), Some(replay));
    }
    chat.handle_server_notification(
        completed(thread, "historical", 5000, 0),
        /*replay_kind*/ None,
    );
    for notification in [
        completed(ThreadId::new(), "foreign", 100, 0),
        completed(thread, "empty", 0, 0),
    ] {
        chat.handle_server_notification(notification, /*replay_kind*/ None);
    }
    chat.add_status_output(
        /*refreshing_rate_limits*/ false, /*request_id*/ None,
    );
    while let Ok(event) = events.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            rendered.extend(
                cell.display_lines(100)
                    .iter()
                    .map(ToString::to_string)
                    .filter(|line| line.contains("Cache:") || line.contains("Prompt cache miss")),
            );
        }
    }
    assert_eq!(
        chat.prompt_cache.status(),
        "80.0% of reported input this session"
    );
    assert_eq!(
        rendered
            .iter()
            .filter(|line| line.contains("Prompt cache miss"))
            .count(),
        1
    );
    insta::assert_snapshot!(rendered.join("\n"));
}

#[test]
fn prompt_cache_missing_usage_stays_unknown_and_never_becomes_a_miss() {
    let thread = ThreadId::new();
    for raw in [
        json!(null),
        json!({}),
        json!({"input_tokens": 100}),
        json!({"input_tokens": 100, "input_tokens_details": {}}),
        json!({"input_tokens": 100, "input_tokens_details": {"cached_tokens": null}}),
    ] {
        let cache = PromptCache::default();
        let ServerNotification::RawResponseCompleted(mut completion) =
            completed(thread, "response", 100, 0)
        else {
            unreachable!()
        };
        completion.usage_metadata.as_mut().unwrap().metadata = Some(raw);
        cache.observe_completion(&completion);
        assert_eq!(
            (cache.status(), cache.take_miss("response")),
            (
                "Unknown (no reported cache usage this session)".into(),
                false
            )
        );
    }
}
