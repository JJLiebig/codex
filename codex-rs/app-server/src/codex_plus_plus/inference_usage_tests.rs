use super::*;
use crate::outgoing_message::OutgoingEnvelope;
use crate::outgoing_message::OutgoingMessage;
use crate::outgoing_message::OutgoingMessageSender;
use codex_protocol::inference_attribution::InferenceAttribution;
use codex_protocol::protocol::TokenUsageInfo;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn canonical_usage_and_completion_preserve_source_without_fabricating_usage() {
    let thread = ThreadId::new();
    let state = Arc::new(Mutex::new(ThreadState::default()));
    let (tx, mut rx) = tokio::sync::mpsc::channel(/*buffer*/ 8);
    let outgoing = ThreadScopedOutgoingMessageSender::new(
        Arc::new(OutgoingMessageSender::new(
            tx,
            codex_analytics::AnalyticsEventsClient::disabled(),
        )),
        vec![crate::outgoing_message::ConnectionId(1)],
        thread,
    );
    for has_usage in [false, true] {
        let attribution = Some(InferenceAttribution::Claude);
        handle_token_count_event(
            thread,
            "turn".into(),
            TokenCountEvent {
                inference_attribution: attribution.clone(),
                info: has_usage.then(|| TokenUsageInfo {
                    total_token_usage: Default::default(),
                    last_token_usage: Default::default(),
                    model_context_window: None,
                }),
                rate_limits: has_usage
                    .then(|| serde_json::from_value(serde_json::json!({})).unwrap()),
            },
            &outgoing,
        )
        .await;
        handle_turn_complete(
            thread,
            "turn".into(),
            TurnCompleteEvent {
                root_turn_id: None,
                inference_attribution: attribution.clone(),
                turn_id: "turn".into(),
                last_agent_message: None,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
                time_to_first_token_ms: None,
            },
            &outgoing,
            &state,
        )
        .await;
        let mut received = Vec::new();
        while let Ok(envelope) = rx.try_recv() {
            let message = match envelope {
                OutgoingEnvelope::Broadcast { message }
                | OutgoingEnvelope::ToConnection { message, .. } => message,
            };
            let OutgoingMessage::AppServerNotification(envelope) = message else {
                panic!("notification")
            };
            received.push(match envelope.notification {
                ServerNotification::ThreadTokenUsageUpdated(event) => {
                    assert_eq!(
                        (event.thread_id, event.turn_id, event.inference_attribution),
                        (thread.to_string(), "turn".into(), attribution.clone())
                    );
                    "usage"
                }
                ServerNotification::AccountRateLimitsUpdated(event) => {
                    assert_eq!(
                        event.inference,
                        Some(codex_protocol::inference_attribution::InferenceScope {
                            thread_id: thread.to_string(),
                            turn_id: "turn".into(),
                            attribution: InferenceAttribution::Claude,
                        })
                    );
                    "quota"
                }
                ServerNotification::TurnCompleted(event) => {
                    assert_eq!(
                        (event.turn.status, event.turn.inference_attribution),
                        (TurnStatus::Completed, attribution.clone())
                    );
                    "complete"
                }
                other => panic!("unexpected {other:?}"),
            });
        }
        assert_eq!(
            received,
            if has_usage {
                vec!["usage", "quota", "complete"]
            } else {
                vec!["complete"]
            }
        );
    }
}
