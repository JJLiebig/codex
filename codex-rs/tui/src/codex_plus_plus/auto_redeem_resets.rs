use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use chrono::DateTime;
use chrono::Utc;
use codex_backend_client::Client as BackendClient;
use codex_backend_client::ConsumeRateLimitResetCreditCode;
use codex_backend_client::RateLimitResetCreditDetails;
use codex_backend_client::RateLimitResetCreditsDetails;
use codex_backend_client::RateLimitsWithResetCredits;
use codex_config::AutoRedeemResets;
use codex_config::ConfigLayerStack;
use codex_login::AccountId;
use codex_login::AccountStore;
use codex_login::AuthCredentialsStoreMode;
use codex_login::CodexAuth;
use codex_login::ResetAttemptPhase;
use codex_login::ResetCredentialSource;
use codex_login::ResetMutationLease;
use codex_login::refresh_auth_from_storage;
use codex_login::token_data::TokenData;
use codex_model_provider::WeeklyWindowPingOutcome;
use codex_model_provider::ping_weekly_window;
use codex_protocol::protocol::RateLimitReachedType;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use sha2::Digest as _;
use sha2::Sha256;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::account_usage;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::codex_plus_plus::native_account_maintenance;
use crate::history_cell;
use crate::legacy_core::config::Config;

const MINUTES_PER_WEEK: i64 = 7 * 24 * 60;

pub(super) enum FreshRedemption {
    Allowed,
    RecoveryOnly,
}
impl FreshRedemption {
    fn allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }
}
struct ResetAccount<'a> {
    config: &'a Config,
    store: &'a AccountStore,
    id: &'a AccountId,
    home: PathBuf,
    client: BackendClient,
}
pub(crate) fn settings(config: &ConfigLayerStack) -> Option<AutoRedeemResets> {
    config
        .effective_user_config()?
        .get("auto_redeem_resets")
        .cloned()
        .and_then(|value| value.try_into().ok())
        .filter(|settings: &AutoRedeemResets| {
            settings.before_expiry_minutes.is_some()
                || settings.weekly_exhausted_min_wait_hours.is_some()
        })
}
pub(super) async fn process_account(
    config: &Config,
    store: &AccountStore,
    account_id: &AccountId,
    settings: AutoRedeemResets,
    fresh_redemption: &FreshRedemption,
) -> Result<()> {
    let Some(mut lease) = store.try_acquire_reset_mutation_lease(account_id)? else {
        return Ok(());
    };
    let phase = lease.state()?.phase;
    if matches!(phase, Some(ResetAttemptPhase::ManualRedeeming { .. })) {
        return Ok(());
    }
    let account = load_reset_account(config, store, account_id).await?;
    match phase {
        Some(ResetAttemptPhase::ManualRedeeming { .. }) => Ok(()),
        Some(ResetAttemptPhase::ActivatingWeekly) => account.recover(&mut lease).await,
        Some(ResetAttemptPhase::Redeeming {
            credit_id: id,
            redeem_request_id: request_id,
        }) => account.redeem(&mut lease, &id, &request_id).await,
        None if !fresh_redemption.allowed() => Ok(()),
        None => {
            let usage = account.client.get_rate_limits_with_reset_credits().await?;
            let credits = account.client.list_rate_limit_reset_credits().await?;
            let now = Utc::now().timestamp();
            let Some(credit_id) = select_credit(&credits, &usage, settings, now) else {
                return Ok(());
            };
            let ResetAttemptPhase::Redeeming {
                credit_id,
                redeem_request_id,
            } = lease.load_or_begin(&credit_id)?
            else {
                anyhow::bail!("fresh reset attempt unexpectedly entered weekly activation");
            };
            account
                .redeem(&mut lease, &credit_id, &redeem_request_id)
                .await
        }
    }
}

pub(super) async fn activate_weekly(
    config: &Config,
    store: &AccountStore,
    account_id: &AccountId,
    _lease: &ResetMutationLease,
) -> WeeklyWindowPingOutcome {
    match load_reset_account(config, store, account_id).await {
        Ok(account) => account.activate_weekly().await,
        Err(err) if account_usage::login_required(&err) => WeeklyWindowPingOutcome::LoginRequired,
        Err(_) => WeeklyWindowPingOutcome::LocalSetup,
    }
}

async fn load_reset_account<'a>(
    config: &'a Config,
    store: &'a AccountStore,
    account_id: &'a AccountId,
) -> Result<ResetAccount<'a>> {
    let account_home = current_account_home(store, account_id)?;
    let auth_config = config.auth_config();
    ensure!(
        auth_config
            .is_login_method_allowed(codex_protocol::config_types::ForcedLoginMethod::Chatgpt),
        "managed authentication policy does not permit ChatGPT accounts"
    );
    let effective_chatgpt_workspaces = auth_config.effective_chatgpt_workspaces();
    let auth_route_config = config.auth_route_config();
    let auth = match refresh_auth_from_storage(
        &account_home,
        AuthCredentialsStoreMode::File,
        effective_chatgpt_workspaces.as_deref(),
        Some(&config.chatgpt_base_url),
        config.auth_keyring_backend_kind(),
        &auth_route_config,
    )
    .await
    {
        Ok(auth) => auth,
        Err(err) => {
            let attempted_auth = err.attempted_auth().cloned();
            let err = anyhow::Error::new(err);
            if account_usage::login_required(&err)
                && let Some(attempted_auth) = attempted_auth
                && let Err(mark_err) =
                    store.record_login_required_if_auth_matches(account_id, &attempted_auth)
            {
                tracing::warn!(%account_id, %mark_err, "failed to record login requirement");
            }
            return Err(err);
        }
    }
    .context("imported account is not authenticated")?;
    ensure!(
        matches_profile(account_id, &auth),
        "imported account identity no longer matches its profile"
    );
    Ok(ResetAccount {
        config,
        store,
        id: account_id,
        home: account_home,
        client: BackendClient::from_auth(
            &config.chatgpt_base_url,
            &auth,
            auth_route_config.http_client_factory().clone(),
        ),
    })
}
fn current_account_home(store: &AccountStore, account_id: &AccountId) -> Result<PathBuf> {
    store
        .enabled_file_accounts()?
        .into_iter()
        .find_map(|(id, home)| (id == *account_id).then_some(home))
        .context("imported account is no longer enabled, signed in, or available")
}
fn matches_profile(account_id: &AccountId, auth: &CodexAuth) -> bool {
    auth.is_chatgpt_auth()
        && auth.uses_codex_backend()
        && auth
            .get_token_data()
            .ok()
            .and_then(|tokens| profile_id(&tokens))
            .is_some_and(|id| account_id.as_str() == id)
}
fn profile_id(tokens: &TokenData) -> Option<String> {
    let (kind, value) = tokens
        .account_id
        .as_ref()
        .or(tokens.id_token.chatgpt_account_id.as_ref())
        .map(|value| ("account", value.clone()))
        .or_else(|| {
            tokens
                .id_token
                .chatgpt_user_id
                .as_ref()
                .map(|value| ("user", value.clone()))
        })
        .or_else(|| {
            tokens
                .id_token
                .email
                .as_ref()
                .map(|value| ("email", value.to_ascii_lowercase()))
        })?;
    let digest = Sha256::digest(format!("{kind}:{value}"));
    format!("acct_{digest:x}").get(..21).map(str::to_string)
}

impl ResetAccount<'_> {
    async fn redeem(
        &self,
        lease: &mut ResetMutationLease,
        credit_id: &str,
        request_id: &str,
    ) -> Result<()> {
        let response = self
            .client
            .consume_rate_limit_reset_credit_by_id(request_id, credit_id)
            .await?;
        match response.code {
            ConsumeRateLimitResetCreditCode::Reset
            | ConsumeRateLimitResetCreditCode::AlreadyRedeemed => {
                ensure!(
                    lease.confirm_redeemed(
                        request_id,
                        Utc::now().timestamp_nanos_opt().unwrap_or(i64::MAX),
                        self.config
                            .model_provider
                            .is_cli_proxy()
                            .then_some(ResetCredentialSource::Imported),
                    )?,
                    "reset attempt changed while its mutation lease was held"
                );
                self.recover(lease).await
            }
            ConsumeRateLimitResetCreditCode::NothingToReset
            | ConsumeRateLimitResetCreditCode::NoCredit => {
                ensure!(
                    lease.clear_redeeming(request_id)?,
                    "reset attempt changed while its mutation lease was held"
                );
                Ok(())
            }
        }
    }

    async fn recover(&self, lease: &mut ResetMutationLease) -> Result<()> {
        let usage = self.client.get_rate_limits_with_reset_credits().await?;
        match exact_weekly(&usage).map(|(_, window)| window) {
            Some(window) if window.used_percent == 0.0 => {
                self.store
                    .record_usage_limit_resets_at(self.id, Utc::now().timestamp())?;
            }
            Some(window) if window.used_percent < 100.0 => {
                self.store
                    .record_usage_limit_resets_at(self.id, Utc::now().timestamp())?;
                lease.finish_weekly_activation()?;
                return Ok(());
            }
            Some(_) => return Ok(()),
            None => return Ok(()),
        }
        let outcome = self.activate_weekly().await;
        if outcome == WeeklyWindowPingOutcome::Completed {
            lease.finish_weekly_activation()?;
        }
        Ok(())
    }

    async fn activate_weekly(&self) -> WeeklyWindowPingOutcome {
        ping_weekly_window(native_account_maintenance::ping_request(
            self.config,
            self.home.clone(),
        ))
        .await
    }
}
fn select_credit(
    credits: &RateLimitResetCreditsDetails,
    usage: &RateLimitsWithResetCredits,
    settings: AutoRedeemResets,
    now: i64,
) -> Option<String> {
    let exhaustion_eligible = settings
        .weekly_exhausted_min_wait_hours
        .is_some_and(|hours| weekly_exhausted(usage, now, hours.get()));
    let expiry_limit = settings.before_expiry_minutes.map(|minutes| {
        now.saturating_add(
            i64::try_from(minutes.get())
                .unwrap_or(i64::MAX)
                .saturating_mul(60),
        )
    });
    credits
        .credits
        .iter()
        .filter_map(|credit| parsed_credit(credit, &credits.credits, now))
        .filter(|(_, expiry)| {
            exhaustion_eligible
                || expiry
                    .zip(expiry_limit)
                    .is_some_and(|(expiry, limit)| expiry <= limit)
        })
        .min_by_key(|(id, expiry)| (expiry.is_none(), expiry.unwrap_or(i64::MAX), id.to_string()))
        .map(|(id, _)| id.to_string())
}
fn parsed_credit<'a>(
    credit: &'a RateLimitResetCreditDetails,
    all: &[RateLimitResetCreditDetails],
    now: i64,
) -> Option<(&'a str, Option<i64>)> {
    if credit.id.is_empty()
        || credit.id.trim() != credit.id
        || credit.reset_type != "codex_rate_limits"
        || credit.status != "available"
        || all.iter().filter(|other| other.id == credit.id).count() != 1
    {
        return None;
    }
    let expiry = credit
        .expires_at
        .as_deref()
        .map(DateTime::parse_from_rfc3339)
        .transpose()
        .ok()?
        .map(|expiry| expiry.timestamp());
    expiry
        .is_none_or(|expiry| expiry > now)
        .then_some((credit.id.as_str(), expiry))
}
fn weekly_exhausted(usage: &RateLimitsWithResetCredits, now: i64, min_wait_hours: u64) -> bool {
    let Some((snapshot, window)) = exact_weekly(usage) else {
        return false;
    };
    snapshot.rate_limit_reached_type == Some(RateLimitReachedType::RateLimitReached)
        && window.used_percent >= 100.0
        && window.resets_at.is_some_and(|resets_at| {
            resets_at.saturating_sub(now)
                >= i64::try_from(min_wait_hours.saturating_mul(60 * 60)).unwrap_or(i64::MAX)
        })
}
fn exact_weekly(
    usage: &RateLimitsWithResetCredits,
) -> Option<(&RateLimitSnapshot, &RateLimitWindow)> {
    let mut matches = usage
        .rate_limits
        .iter()
        .filter(|snapshot| snapshot.limit_id.as_deref() == Some("codex"));
    let snapshot = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let mut windows = [snapshot.primary.as_ref(), snapshot.secondary.as_ref()]
        .into_iter()
        .flatten()
        .filter(|window| window.window_minutes == Some(MINUTES_PER_WEEK));
    let window = windows.next()?;
    if windows.next().is_some() {
        return None;
    }
    (window.used_percent.is_finite() && window.used_percent >= 0.0).then_some((snapshot, window))
}
pub(super) struct CompletionNotices {
    started_at: i64,
    seen: HashMap<AccountId, String>,
}
impl CompletionNotices {
    pub(super) fn new() -> Self {
        Self {
            started_at: Utc::now().timestamp_nanos_opt().unwrap_or(i64::MAX),
            seen: HashMap::new(),
        }
    }
    // Each already-open same-home process reports a completion once.
    pub(super) fn poll(&mut self, store: &AccountStore, tx: &AppEventSender) {
        let Ok(profiles) = store.list() else {
            return;
        };
        for profile in profiles {
            let Some(state) = store
                .try_acquire_reset_mutation_lease(&profile.id)
                .ok()
                .flatten()
                .and_then(|lease| lease.state().ok())
            else {
                continue;
            };
            let Some(completion) = state.completion else {
                continue;
            };
            if completion.manual {
                continue;
            }
            // Reconcile readiness on existing scheduler scans, even after a notice was shown.
            // Redemption can succeed while quota recovery is still pending.
            if state.phase.is_none() && completion.completed_at >= self.started_at {
                tx.send(AppEvent::UsageResetCompleted {
                    account_id: profile.id.clone(),
                    completed_at: completion.completed_at,
                });
            }
            if self.seen.get(&profile.id) == Some(&completion.id) {
                continue;
            }
            self.seen.insert(profile.id.clone(), completion.id);
            if completion.completed_at >= self.started_at {
                tx.send(AppEvent::InsertHistoryCell(Box::new(notice_cell(
                    &profile.label,
                ))));
            }
        }
    }
}

fn notice_cell(label: &str) -> impl history_cell::HistoryCell + use<> {
    history_cell::new_info_event(
        format!("Codex++ auto-redeemed a usage reset for {label}."),
        /*hint*/ None,
    )
}

#[cfg(test)]
#[path = "auto_redeem_resets_tests.rs"]
mod tests;
