use super::*;
use crate::app::test_support::make_test_app;
use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
use pretty_assertions::assert_eq;
use serde_json::json;

#[tokio::test]
async fn prompt_cache_survives_background_completion_rebuild_and_switch() {
    let mut app = make_test_app().await;
    let first = ThreadId::new();
    let second = ThreadId::new();
    let completion: codex_app_server_protocol::ServerNotification = serde_json::from_value(json!({
        "method": "rawResponse/completed", "params": {
            "threadId": first.to_string(), "turnId": "turn", "responseId": "response",
            "usage": {"totalTokens": 100, "inputTokens": 100,
                "cachedInputTokens": 75, "outputTokens": 0, "reasoningOutputTokens": 0},
            "usageMetadata": {"amount": null, "metadata": {
                "input_tokens": 100, "input_tokens_details": {"cached_tokens": 75}
            }}
        }
    }))
    .unwrap();
    app.ensure_thread_channel(first)
        .store
        .lock()
        .await
        .push_notification(completion.clone());
    app.activate_thread_channel(first).await;
    assert_eq!(
        (
            app.chat_widget.prompt_cache.status(),
            app.chat_widget.prompt_cache.status_line()
        ),
        (
            "75.0% of reported input this session".into(),
            ("Cache 75.0%".into(), None)
        )
    );
    app.store_active_thread_receiver().await;
    app.clear_active_thread().await;
    // The normal switch recreates the widget and later attaches the same per-thread store.
    let (replacement, _sender, _events, _ops) = make_chatwidget_manual_with_sender().await;
    app.replace_chat_widget(replacement);
    app.ensure_thread_channel(second);
    app.activate_thread_channel(second).await;
    assert_eq!(
        (
            app.chat_widget.prompt_cache.status(),
            app.chat_widget.prompt_cache.status_line()
        ),
        (
            "Unknown (no reported cache usage this session)".into(),
            ("Cache unknown".into(), None)
        )
    );
    app.store_active_thread_receiver().await;
    app.clear_active_thread().await;
    app.activate_thread_channel(first).await;
    app.ensure_thread_channel(first)
        .store
        .lock()
        .await
        .push_notification(completion.clone());
    assert_eq!(
        (
            app.chat_widget.prompt_cache.status(),
            app.chat_widget.prompt_cache.status_line()
        ),
        (
            "75.0% of reported input this session".into(),
            ("Cache 75.0%".into(), None)
        )
    );
    let reconnected_cache = app.prompt_cache_for_thread(first).await;
    app.clear_active_thread().await;
    app.thread_event_channels.remove(&first);
    app.ensure_thread_channel(first)
        .store
        .lock()
        .await
        .prompt_cache = reconnected_cache;
    app.activate_thread_channel(first).await;
    assert_eq!(
        (
            app.chat_widget.prompt_cache.status(),
            app.chat_widget.prompt_cache.status_line()
        ),
        (
            "75.0% of reported input this session".into(),
            ("Cache 75.0%".into(), None)
        )
    );
    let codex_app_server_protocol::ServerNotification::RawResponseCompleted(mut miss) = completion
    else {
        unreachable!()
    };
    miss.response_id = "background-miss".into();
    miss.usage.as_mut().unwrap().cached_input_tokens = 0;
    miss.usage_metadata
        .as_mut()
        .unwrap()
        .metadata
        .as_mut()
        .unwrap()["input_tokens_details"]["cached_tokens"] = json!(0);
    // A background miss changes the latest percentage without a delayed warning on activation.
    app.store_active_thread_receiver().await;
    app.clear_active_thread().await;
    app.ensure_thread_channel(first)
        .store
        .lock()
        .await
        .push_notification(
            codex_app_server_protocol::ServerNotification::RawResponseCompleted(miss),
        );
    app.activate_thread_channel(first).await;
    assert_eq!(
        app.chat_widget.prompt_cache.status_line(),
        ("Cache 0.0%".into(), None)
    );
}
