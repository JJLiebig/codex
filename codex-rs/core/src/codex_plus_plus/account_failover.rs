//! Observable usage-limit failover for imported Codex++ accounts.

use std::collections::HashSet;

use crate::client::ModelClientSession;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use codex_config::types::AutomaticAccountSelection;
use codex_login::AccountId;
use codex_login::AuthManager;
use codex_login::auth::ImportedAccountSwitchOutcome;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::UsageLimitReachedError;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::StreamErrorEvent;

#[derive(Default)]
pub(crate) struct UsageLimitFailoverTracking {
    pub(crate) attempted_account_ids: HashSet<String>,
    pub(crate) selected_account_ids: Vec<AccountId>,
}

pub(crate) enum UsageLimitFailoverOutcome {
    Retried,
    Unavailable,
    RequestAccountChanged,
}

pub(crate) async fn report_tracked_client_failovers(
    client_session: &mut ModelClientSession,
    attempted_account_ids: &mut HashSet<String>,
    sess: &Session,
    turn_context: &TurnContext,
) {
    let tracking = client_session.take_usage_limit_failover_tracking();
    attempted_account_ids.extend(tracking.attempted_account_ids);
    let Some(auth_manager) = turn_context.auth_manager.as_ref() else {
        return;
    };
    for account_id in tracking.selected_account_ids {
        report_switch(sess, turn_context, auth_manager, &account_id).await;
    }
}

pub(crate) async fn switch_and_report(
    client_session: &mut ModelClientSession,
    attempted_account_ids: &mut HashSet<String>,
    sess: &Session,
    turn_context: &TurnContext,
    usage_limit: &UsageLimitReachedError,
) -> UsageLimitFailoverOutcome {
    if let Some(rate_limits) = usage_limit.rate_limits.clone() {
        sess.update_rate_limits(turn_context, *rate_limits).await;
    }
    let Some(auth_manager) = turn_context.auth_manager.as_ref() else {
        return UsageLimitFailoverOutcome::Unavailable;
    };
    if client_session.request_account_id() != auth_manager.active_account_id() {
        return UsageLimitFailoverOutcome::RequestAccountChanged;
    }
    if let Some(account_id) = auth_manager.active_account_id() {
        if let Some(resets_at) = usage_limit.resets_at.as_ref()
            && let Err(err) = auth_manager
                .record_imported_account_usage_limit_resets_at(&account_id, resets_at.timestamp())
        {
            tracing::warn!("failed to record account usage limit reset: {err}");
        }
        attempted_account_ids.insert(account_id.to_string());
    }

    let outcome = auth_manager
        .switch_to_next_imported_account(attempted_account_ids)
        .await;
    if outcome != ImportedAccountSwitchOutcome::ReadyToRetry {
        return UsageLimitFailoverOutcome::Unavailable;
    }

    client_session.reset_websocket_session();
    if let Some(account_id) = auth_manager.active_account_id() {
        report_switch(sess, turn_context, auth_manager, &account_id).await;
    }
    UsageLimitFailoverOutcome::Retried
}

pub(crate) fn manual_selection_error(
    turn_context: &TurnContext,
    usage_limit: &UsageLimitReachedError,
) -> Option<CodexErr> {
    let auth_manager = turn_context.auth_manager.as_ref()?;
    if auth_manager.automatic_account_selection() != AutomaticAccountSelection::Disabled
        || auth_manager.active_account_id().is_none()
    {
        return None;
    }

    let guidance = "Automatic account selection is disabled. Choose another account in the Codex TUI or enable automatic account selection";
    let promo_message = Some(match usage_limit.promo_message.as_ref() {
        Some(promo_message) => format!("{promo_message}\n\n{guidance}"),
        None => guidance.to_string(),
    });
    Some(CodexErr::new(CodexErrorDetails::UsageLimitReached(
        UsageLimitReachedError {
            plan_type: usage_limit.plan_type.clone(),
            resets_at: usage_limit.resets_at,
            rate_limits: usage_limit.rate_limits.clone(),
            promo_message,
            rate_limit_reached_type: usage_limit.rate_limit_reached_type,
        },
    )))
}

async fn report_switch(
    sess: &Session,
    turn_context: &TurnContext,
    auth_manager: &AuthManager,
    account_id: &AccountId,
) {
    let label = auth_manager
        .account_candidates()
        .ok()
        .and_then(|accounts| {
            accounts
                .into_iter()
                .find(|account| &account.id == account_id)
        })
        .map_or_else(|| account_id.to_string(), |account| account.display_label);
    for message in [
        "Selecting a replacement account...".to_string(),
        format!("Retrying with {label}..."),
    ] {
        sess.send_event(
            turn_context,
            EventMsg::StreamError(StreamErrorEvent {
                message,
                codex_error_info: Some(CodexErrorInfo::UsageLimitExceeded),
                additional_details: Some("The previous account reached its usage limit.".into()),
            }),
        )
        .await;
    }
}
