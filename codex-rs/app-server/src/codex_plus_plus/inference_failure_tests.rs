use super::*;
use crate::bespoke_event_handling::handle_error_notification;
use crate::bespoke_event_handling::handle_turn_complete;
use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::OutgoingEnvelope;
use crate::outgoing_message::OutgoingMessage;
use crate::outgoing_message::OutgoingMessageSender;
use crate::outgoing_message::ThreadScopedOutgoingMessageSender;
use crate::thread_state::ThreadState;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::TurnStatus;
use codex_protocol::ThreadId;
use codex_protocol::inference_attribution::InferenceAttribution;
use codex_protocol::protocol::TurnCompleteEvent;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc;

#[tokio::test]
async fn inference_failure_survives_live_notification_and_completed_fallback() {
    for live in [true, false] {
        let thread = ThreadId::new();
        let state = Arc::new(Mutex::new(ThreadState::default()));
        let (tx, mut rx) = mpsc::channel(/*buffer*/ 8);
        let outgoing = ThreadScopedOutgoingMessageSender::new(
            Arc::new(OutgoingMessageSender::new(
                tx,
                codex_analytics::AnalyticsEventsClient::disabled(),
            )),
            vec![ConnectionId(1)],
            thread,
        );
        let error = ErrorEvent {
            message: "Provider quota reached".into(),
            inference_attribution: Some(InferenceAttribution::Claude),
            codex_error_info: Some(codex_protocol::protocol::CodexErrorInfo::BadRequest),
            misalignment: None,
        };
        assert!(error.affects_turn_status());
        if live {
            handle_error_notification(
                thread,
                "turn",
                error_event(error.clone()),
                &outgoing,
                &state,
            )
            .await;
        }
        handle_turn_complete(
            thread,
            "turn".into(),
            TurnCompleteEvent {
                inference_attribution: None,
                turn_id: "turn".into(),
                last_agent_message: None,
                error: Some(error),
                started_at: None,
                completed_at: Some(20),
                duration_ms: None,
                time_to_first_token_ms: None,
            },
            &outgoing,
            &state,
        )
        .await;
        for completed in if live { vec![false, true] } else { vec![true] } {
            let message = match rx.recv().await.unwrap() {
                OutgoingEnvelope::Broadcast { message }
                | OutgoingEnvelope::ToConnection { message, .. } => message,
            };
            let OutgoingMessage::AppServerNotification(envelope) = message else {
                panic!("notification")
            };
            let error = match envelope.notification {
                ServerNotification::Error(notification) if !completed => notification.error,
                ServerNotification::TurnCompleted(notification) if completed => {
                    assert_eq!(notification.turn.status, TurnStatus::Failed);
                    notification.turn.error.unwrap()
                }
                other => panic!("unexpected {other:?}"),
            };
            assert_eq!(
                error,
                TurnError {
                    message: "Provider quota reached".into(),
                    inference_attribution: Some(InferenceAttribution::Claude),
                    codex_error_info: Some(codex_app_server_protocol::CodexErrorInfo::BadRequest),
                    additional_details: None,
                    misalignment: None,
                }
            );
        }
    }
}
