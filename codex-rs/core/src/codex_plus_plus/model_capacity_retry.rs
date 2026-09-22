use std::time::Duration;

use crate::client::ModelClientSession;
use crate::responses_retry::ResponsesStreamRequest;
use crate::responses_retry::ResponsesStreamRetryState;
use crate::responses_retry::handle_response_stream_error;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use codex_config::ModelCapacityRetryMode;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::WarningEvent;
use tokio_util::sync::CancellationToken;
use tracing::warn;

const RETRY_SCHEDULE: [(Duration, &str); 4] = [
    (Duration::from_secs(60), "1 minute"),
    (Duration::from_secs(2 * 60), "2 minutes"),
    (Duration::from_secs(5 * 60), "5 minutes"),
    (Duration::from_secs(15 * 60), "15 minutes"),
];

pub(crate) fn applies_to_sampling(err: &CodexErr, session_source: &SessionSource) -> bool {
    matches!(err.details(), CodexErrorDetails::ServerOverloaded)
        && !crate::guardian::is_basic_session_source(session_source)
}

pub(crate) async fn handle_sampling_error(
    capacity_retries: &mut u64,
    retry_state: &mut ResponsesStreamRetryState,
    max_retries: u64,
    err: CodexErr,
    client_session: &mut ModelClientSession,
    sess: &Session,
    turn_context: &TurnContext,
    cancellation_token: &CancellationToken,
) -> Result<(), CodexErr> {
    if applies_to_sampling(&err, &turn_context.session_source) {
        handle(
            capacity_retries,
            err,
            client_session,
            sess,
            turn_context,
            cancellation_token,
        )
        .await
    } else {
        handle_response_stream_error(
            retry_state,
            max_retries,
            err,
            client_session,
            sess,
            turn_context,
            ResponsesStreamRequest::Sampling,
        )
        .await
    }
}

pub(crate) async fn handle(
    retries: &mut u64,
    err: CodexErr,
    client_session: &mut ModelClientSession,
    sess: &Session,
    turn_context: &TurnContext,
    cancellation_token: &CancellationToken,
) -> Result<(), CodexErr> {
    let mode = turn_context.config.model_capacity_retry_mode;
    let retry_index = usize::try_from(*retries).unwrap_or(usize::MAX);
    let retry = RETRY_SCHEDULE
        .get(retry_index)
        .copied()
        .or_else(|| (mode == ModelCapacityRetryMode::Indefinite).then_some(RETRY_SCHEDULE[3]));
    let Some((delay, delay_label)) = retry else {
        return Err(err);
    };

    *retries += 1;
    let retry_count = *retries;
    let retry_progress = match mode {
        ModelCapacityRetryMode::Bounded => {
            format!("{retry_count}/{}", RETRY_SCHEDULE.len())
        }
        ModelCapacityRetryMode::Indefinite => format!("retry {retry_count}; indefinite"),
    };
    warn!("model at capacity - retrying sampling request ({retry_progress} in {delay:?})...");
    let sleep = tokio::time::sleep(delay);
    tokio::pin!(sleep);
    sess.send_event(
        turn_context,
        EventMsg::Warning(WarningEvent {
            message: format!(
                "The selected model is at capacity. Retrying in {delay_label} ({retry_progress})."
            ),
        }),
    )
    .await;

    tokio::select! {
        _ = &mut sleep => {
            client_session.reset_websocket_session();
            Ok(())
        },
        _ = cancellation_token.cancelled() => Err(CodexErr::TurnAborted),
    }
}
