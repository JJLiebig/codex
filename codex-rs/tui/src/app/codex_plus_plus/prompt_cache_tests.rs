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
        app.chat_widget.prompt_cache.status(),
        "75.0% of reported input this session"
    );
    app.store_active_thread_receiver().await;
    // The normal switch recreates the widget and later attaches the same per-thread store.
    let (replacement, _sender, _events, _ops) = make_chatwidget_manual_with_sender().await;
    app.replace_chat_widget(replacement);
    app.ensure_thread_channel(second);
    app.activate_thread_channel(second).await;
    assert_eq!(
        app.chat_widget.prompt_cache.status(),
        "Unknown (no reported cache usage this session)"
    );
    app.store_active_thread_receiver().await;
    app.activate_thread_channel(first).await;
    app.ensure_thread_channel(first)
        .store
        .lock()
        .await
        .push_notification(completion);
    assert_eq!(
        app.chat_widget.prompt_cache.status(),
        "75.0% of reported input this session"
    );
    let mut reconnected_store = super::super::ThreadEventStore::new(/*capacity*/ 8);
    app.inherit_prompt_cache(first, &mut reconnected_store)
        .await;
    assert_eq!(
        reconnected_store.prompt_cache.status(),
        "75.0% of reported input this session"
    );
}
