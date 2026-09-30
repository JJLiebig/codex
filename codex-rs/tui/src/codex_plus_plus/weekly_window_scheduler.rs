use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use chrono::Utc;
use codex_config::AutoRedeemResets;
use codex_config::WeeklyUsageWindowAutoStart;
use codex_login::AccountId;
use codex_login::AccountStore;
use codex_login::WeeklyWindowAttemptDecision;
use codex_login::WeeklyWindowAttemptOutcome;
use codex_login::WeeklyWindowError;
use codex_login::WeeklyWindowRetryableError;
use codex_login::WeeklyWindowUsage;
use codex_model_provider::WeeklyWindowPingOutcome;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::account_usage;
use crate::app_event_sender::AppEventSender;
use crate::codex_plus_plus::auto_redeem_resets;
use crate::codex_plus_plus::preflight_native_account_maintenance;
use crate::legacy_core::config::Config;

const SCAN_INTERVAL: Duration = Duration::from_secs(5 * 60);
const RESET_REDEMPTION_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 90);
const MAX_STATUS_ACCOUNTS: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SchedulerSettings {
    weekly: bool,
    auto_redeem: Option<AutoRedeemResets>,
}

impl SchedulerSettings {
    fn enabled(self) -> bool {
        self.weekly || self.auto_redeem.is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WeeklyWindowStatus {
    Waiting(Option<u8>),
    Started(Option<u8>),
    Retrying(Option<u8>),
    SignInRequired,
    Failed,
}

pub(crate) struct WeeklyWindowScheduler {
    state: watch::Sender<SchedulerSettings>,
    statuses: Arc<Mutex<HashMap<AccountId, WeeklyWindowStatus>>>,
    _task: JoinHandle<()>,
}

impl WeeklyWindowScheduler {
    pub(crate) fn spawn(config: Config, app_event_tx: AppEventSender) -> Self {
        let initial = SchedulerSettings {
            weekly: config.weekly_usage_window_auto_start == WeeklyUsageWindowAutoStart::Enabled,
            auto_redeem: auto_redeem_resets::settings(&config.config_layer_stack),
        };
        let (state, receiver) = watch::channel(initial);
        let statuses = Arc::new(Mutex::new(HashMap::new()));
        let task_statuses = Arc::clone(&statuses);
        let notices = Arc::new(Mutex::new(auto_redeem_resets::CompletionNotices::new()));
        let task = tokio::spawn(run_schedule(
            move |scan_control| {
                scan(
                    config.clone(),
                    scan_control,
                    Arc::clone(&task_statuses),
                    Arc::clone(&notices),
                    app_event_tx.clone(),
                )
            },
            receiver,
        ));
        Self {
            state,
            statuses,
            _task: task,
        }
    }

    pub(crate) fn set_settings(&self, weekly: bool, auto_redeem: Option<AutoRedeemResets>) {
        if !weekly {
            self.statuses
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
        }
        let next = SchedulerSettings {
            weekly,
            auto_redeem,
        };
        let _ = self
            .state
            .send_if_modified(|settings| std::mem::replace(settings, next) != next);
    }

    pub(crate) fn statuses(&self) -> HashMap<AccountId, WeeklyWindowStatus> {
        self.statuses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

async fn run_schedule<F, Fut>(mut scan: F, mut control: watch::Receiver<SchedulerSettings>)
where
    F: FnMut(watch::Receiver<SchedulerSettings>) -> Fut,
    Fut: Future<Output = ()>,
{
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now(), SCAN_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = interval.tick() => scan(control.clone()).await,
            changed = control.changed() => match changed {
                Err(_) => return,
                Ok(()) if control.borrow_and_update().enabled() => {
                    scan(control.clone()).await;
                    interval.reset();
                }
                Ok(()) => {}
            }
        }
    }
}

async fn scan(
    config: Config,
    mut control: watch::Receiver<SchedulerSettings>,
    status_sink: Arc<Mutex<HashMap<AccountId, WeeklyWindowStatus>>>,
    notices: Arc<Mutex<auto_redeem_resets::CompletionNotices>>,
    app_event_tx: AppEventSender,
) {
    let store = AccountStore::new(config.codex_home.to_path_buf());
    if let Err(error) = auto_redeem_resets::reconcile_pending(&config, &store).await {
        tracing::warn!(%error, "could not scan confirmed proxy resets");
    }
    poll_notices(&notices, &store, &app_event_tx);
    let mut settings = *control.borrow();
    if !settings.enabled() {
        return;
    }
    let mut fresh_redemption = auto_redeem_resets::FreshRedemption::Allowed;
    if let Err(outcome) = preflight_native_account_maintenance(&config) {
        tracing::warn!(?outcome, "weekly-window scheduler unsupported");
        settings.weekly = false;
        fresh_redemption = auto_redeem_resets::FreshRedemption::RecoveryOnly;
    }
    let _scan_lease = match store.try_acquire_weekly_window_scan() {
        Ok(Some(lease)) => lease,
        Ok(None) => return,
        Err(err) => {
            tracing::warn!(%err, "weekly-window scheduler could not acquire scan lease");
            return;
        }
    };
    let accounts = match store.enabled_file_accounts() {
        Ok(accounts) => accounts,
        Err(err) => {
            tracing::warn!(%err, "weekly-window scheduler could not read imported accounts");
            return;
        }
    };
    if control.has_changed().unwrap_or(true) {
        return;
    }
    if let Some(auto_redeem) = settings.auto_redeem {
        for (account_id, _) in &accounts {
            if control.has_changed().unwrap_or(true) {
                return;
            }
            match tokio::select! {
                result = tokio::time::timeout(
                    RESET_REDEMPTION_TIMEOUT,
                    auto_redeem_resets::process_account(
                        &config,
                        &store,
                        account_id,
                        auto_redeem,
                        &fresh_redemption,
                    ),
                ) => result,
                // Explicit opt-out pauses persisted recovery; re-enabling resumes replay.
                _ = control.changed() => return,
            } {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    tracing::warn!(%account_id, %err, "automatic usage-reset redemption failed");
                }
                Err(_) => {
                    tracing::warn!(%account_id, "automatic usage-reset redemption timed out");
                }
            }
        }
    }
    if !settings.weekly {
        poll_notices(&notices, &store, &app_event_tx);
        return;
    }
    let loaded = account_usage::load(&config, &accounts, &store).await;
    let mut statuses = HashMap::new();
    for (account_id, attempted_auth) in &loaded.login_required {
        record_status(
            &mut statuses,
            account_id,
            WeeklyWindowStatus::SignInRequired,
        );
        if let Err(err) = store.record_login_required_if_auth_matches(account_id, attempted_auth) {
            tracing::warn!(%account_id, %err, "weekly-window scheduler could not record login failure");
        }
    }

    for (account_id, account_home) in accounts {
        if control.has_changed().unwrap_or(true) {
            return;
        }
        if loaded.login_required.contains_key(&account_id) {
            continue;
        }
        let account_usage = loaded.usage.get(&account_id);
        let remaining = account_usage.and_then(|usage| usage.weekly_remaining_percent);
        let usage = weekly_usage(account_usage);
        let attempt = match store.begin_weekly_window_attempt(
            &account_id,
            usage,
            Utc::now().timestamp(),
        ) {
            Ok(WeeklyWindowAttemptDecision::Ready(attempt)) => attempt,
            Ok(WeeklyWindowAttemptDecision::NotDue | WeeklyWindowAttemptDecision::Locked) => {
                record_status(
                    &mut statuses,
                    &account_id,
                    if account_usage.is_some() {
                        WeeklyWindowStatus::Waiting(remaining)
                    } else {
                        WeeklyWindowStatus::Failed
                    },
                );
                continue;
            }
            Ok(WeeklyWindowAttemptDecision::StateUnavailable) => {
                record_status(&mut statuses, &account_id, WeeklyWindowStatus::Failed);
                continue;
            }
            Err(err) => {
                tracing::warn!(%account_id, %err, "weekly-window scheduler could not begin attempt");
                record_status(&mut statuses, &account_id, WeeklyWindowStatus::Failed);
                continue;
            }
        };
        let reset_lease = match store.try_acquire_reset_mutation_lease(&account_id) {
            Ok(lease) => lease,
            Err(err) => {
                tracing::warn!(%account_id, %err, "weekly-window scheduler could not acquire reset authority");
                None
            }
        };
        let Some(reset_lease) = reset_lease else {
            if let Err(err) = attempt.finish(
                WeeklyWindowAttemptOutcome::Retryable {
                    error: WeeklyWindowRetryableError::Transient,
                },
                Utc::now().timestamp(),
            ) {
                tracing::warn!(%account_id, %err, "weekly-window scheduler could not defer activation");
            }
            record_status(
                &mut statuses,
                &account_id,
                WeeklyWindowStatus::Retrying(remaining),
            );
            continue;
        };
        let outcome =
            auto_redeem_resets::activate_weekly(&config, &store, &account_id, &reset_lease).await;
        let (refreshed_usage, refreshed_remaining) = if outcome
            == WeeklyWindowPingOutcome::Completed
        {
            let refreshed =
                account_usage::load(&config, &[(account_id.clone(), account_home)], &store).await;
            let refreshed_account_usage = refreshed.usage.get(&account_id);
            (
                weekly_usage(refreshed_account_usage),
                refreshed_account_usage.and_then(|usage| usage.weekly_remaining_percent),
            )
        } else {
            (WeeklyWindowUsage::Missing, remaining)
        };
        let status = match outcome {
            WeeklyWindowPingOutcome::Completed => WeeklyWindowStatus::Started(refreshed_remaining),
            WeeklyWindowPingOutcome::LoginRequired
            | WeeklyWindowPingOutcome::AuthenticationRecovery => WeeklyWindowStatus::SignInRequired,
            WeeklyWindowPingOutcome::LocalSetup
            | WeeklyWindowPingOutcome::Rejected { .. }
            | WeeklyWindowPingOutcome::Ambiguous { .. } => WeeklyWindowStatus::Retrying(remaining),
            WeeklyWindowPingOutcome::UnsupportedConfiguration
            | WeeklyWindowPingOutcome::UnsupportedRouting => WeeklyWindowStatus::Failed,
        };
        if let Err(err) = attempt.finish(
            attempt_outcome(outcome, refreshed_usage),
            Utc::now().timestamp(),
        ) {
            tracing::warn!(%account_id, %err, "weekly-window scheduler could not finish attempt");
            record_status(&mut statuses, &account_id, WeeklyWindowStatus::Failed);
        } else {
            record_status(&mut statuses, &account_id, status);
        }
    }
    if !control.has_changed().unwrap_or(true) {
        *status_sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = statuses;
    }
    poll_notices(&notices, &store, &app_event_tx);
}

fn poll_notices(
    notices: &Mutex<auto_redeem_resets::CompletionNotices>,
    store: &AccountStore,
    app_event_tx: &AppEventSender,
) {
    notices
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .poll(store, app_event_tx);
}

fn record_status(
    statuses: &mut HashMap<AccountId, WeeklyWindowStatus>,
    account_id: &AccountId,
    status: WeeklyWindowStatus,
) {
    if statuses.len() < MAX_STATUS_ACCOUNTS || statuses.contains_key(account_id) {
        statuses.insert(account_id.clone(), status);
    }
}

fn weekly_usage(usage: Option<&account_usage::AccountUsage>) -> WeeklyWindowUsage {
    let Some(usage) = usage else {
        return WeeklyWindowUsage::Missing;
    };
    match usage.weekly_unused {
        Some(unused) => WeeklyWindowUsage::Present {
            unused,
            resets_at: usage.weekly_reset_at,
        },
        None => WeeklyWindowUsage::Missing,
    }
}

fn attempt_outcome(
    outcome: WeeklyWindowPingOutcome,
    refreshed_usage: WeeklyWindowUsage,
) -> WeeklyWindowAttemptOutcome {
    match outcome {
        WeeklyWindowPingOutcome::Completed => {
            WeeklyWindowAttemptOutcome::Completed { refreshed_usage }
        }
        WeeklyWindowPingOutcome::LocalSetup => WeeklyWindowAttemptOutcome::Retryable {
            error: WeeklyWindowRetryableError::LocalSetup,
        },
        WeeklyWindowPingOutcome::Rejected { status } => WeeklyWindowAttemptOutcome::Retryable {
            error: WeeklyWindowRetryableError::Rejected { status },
        },
        WeeklyWindowPingOutcome::LoginRequired => WeeklyWindowAttemptOutcome::Retryable {
            error: WeeklyWindowRetryableError::LoginRequired,
        },
        WeeklyWindowPingOutcome::AuthenticationRecovery => WeeklyWindowAttemptOutcome::Retryable {
            error: WeeklyWindowRetryableError::AuthenticationRecovery,
        },
        WeeklyWindowPingOutcome::Ambiguous { status } => {
            WeeklyWindowAttemptOutcome::Ambiguous { status }
        }
        WeeklyWindowPingOutcome::UnsupportedConfiguration => {
            WeeklyWindowAttemptOutcome::Unsupported {
                error: WeeklyWindowError::UnsupportedConfiguration,
            }
        }
        WeeklyWindowPingOutcome::UnsupportedRouting => WeeklyWindowAttemptOutcome::Unsupported {
            error: WeeklyWindowError::UnsupportedRouting,
        },
    }
}

#[cfg(test)]
#[path = "weekly_window_scheduler_tests.rs"]
mod tests;
