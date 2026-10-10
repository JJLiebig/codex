use crate::legacy_core::config::Config;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_backend_client::Client as BackendClient;
use codex_backend_client::RateLimitsWithResetCredits;
use codex_backend_client::RequestError;
use codex_login::AccountId;
use codex_login::AccountStore;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthDotJson;
use codex_login::AuthRouteConfig;
use codex_login::CodexAuth;
use codex_login::refresh_auth_from_storage;
use codex_protocol::auth::RefreshTokenFailedError;
use codex_protocol::auth::RefreshTokenFailedReason;
use codex_protocol::protocol::RateLimitSnapshot;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::task::JoinSet;

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const MINUTES_PER_WEEK: i64 = 7 * 24 * 60;

#[path = "codex_plus_plus/account_usage_cooldown.rs"]
mod cooldown;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AccountUsage {
    pub(crate) plan_type: Option<codex_protocol::account::PlanType>,
    pub(crate) available_resets: Option<i64>,
    pub(crate) primary_window_minutes: Option<i64>,
    pub(crate) five_hour_reset_at: Option<i64>,
    pub(crate) five_hour_remaining_percent: Option<u8>,
    pub(crate) five_hour_exhausted: bool,
    pub(crate) weekly_reset_at: Option<i64>,
    pub(crate) weekly_unused: Option<bool>,
    pub(crate) weekly_remaining_percent: Option<u8>,
    pub(crate) weekly_exhausted: bool,
}

impl AccountUsage {
    fn exhausted_until(self) -> Option<i64> {
        [
            self.five_hour_reset_at.filter(|_| self.five_hour_exhausted),
            self.weekly_reset_at.filter(|_| self.weekly_exhausted),
        ]
        .into_iter()
        .flatten()
        .max()
    }
}

#[derive(Default)]
pub(crate) struct AccountUsageLoad {
    pub(crate) usage: HashMap<AccountId, AccountUsage>,
    pub(crate) login_required: HashMap<AccountId, AuthDotJson>,
}

struct AccountUsageFetchError {
    error: anyhow::Error,
    attempted_auth: Option<AuthDotJson>,
}

impl AccountUsageFetchError {
    fn new(error: impl Into<anyhow::Error>) -> Self {
        Self {
            error: error.into(),
            attempted_auth: None,
        }
    }
}

pub(crate) async fn load(
    config: &Config,
    accounts: &[(AccountId, PathBuf)],
    store: &AccountStore,
) -> AccountUsageLoad {
    let mut tasks = JoinSet::new();
    for (account_id, account_home) in accounts {
        let account_id = account_id.clone();
        let account_home = account_home.clone();
        let config = config.clone();
        tasks.spawn(async move {
            let result = tokio::time::timeout(FETCH_TIMEOUT, fetch(account_home, config))
                .await
                .map_err(|_| AccountUsageFetchError::new(anyhow!("rate-limit request timed out")))
                .and_then(std::convert::identity);
            (account_id, result)
        });
    }

    let mut loaded = AccountUsageLoad::default();
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok((account_id, Ok(account_usage))) => {
                if let Some(resets_at) = cooldown::reset_at(account_usage)
                    && let Err(err) = store.record_usage_limit_resets_at(&account_id, resets_at)
                {
                    tracing::warn!(
                        %account_id,
                        %err,
                        "failed to persist imported account usage limit reset"
                    );
                }
                loaded.usage.insert(account_id, account_usage);
            }
            Ok((account_id, Err(err))) => {
                if login_required(&err.error)
                    && let Some(attempted_auth) = err.attempted_auth
                {
                    loaded
                        .login_required
                        .insert(account_id.clone(), attempted_auth);
                }
                tracing::warn!(%account_id, %err.error, "failed to load imported account usage");
            }
            Err(err) => tracing::warn!(%err, "imported account usage task failed"),
        }
    }
    loaded
}

async fn fetch(
    account_home: PathBuf,
    config: Config,
) -> std::result::Result<AccountUsage, AccountUsageFetchError> {
    let auth_config = config.auth_config();
    if !auth_config
        .is_login_method_allowed(codex_protocol::config_types::ForcedLoginMethod::Chatgpt)
    {
        return Err(AccountUsageFetchError::new(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "managed authentication policy does not permit ChatGPT accounts",
        )));
    }
    let effective_chatgpt_workspaces = auth_config.effective_chatgpt_workspaces();
    let auth_route_config = config.auth_route_config();
    let auth = CodexAuth::from_auth_storage(
        &account_home,
        AuthCredentialsStoreMode::File,
        effective_chatgpt_workspaces.as_deref(),
        Some(&config.chatgpt_base_url),
        config.auth_keyring_backend_kind(),
        &auth_route_config,
    )
    .await
    .map_err(AccountUsageFetchError::new)?
    .context("imported account is not authenticated")
    .map_err(AccountUsageFetchError::new)?;

    match fetch_with_auth(&auth, &config.chatgpt_base_url, &auth_route_config).await {
        Err(err) if is_unauthorized(&err) => {
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
                    return Err(AccountUsageFetchError {
                        error: err.into(),
                        attempted_auth,
                    });
                }
            }
            .context("imported account is not authenticated")
            .map_err(AccountUsageFetchError::new)?;
            fetch_with_auth(&auth, &config.chatgpt_base_url, &auth_route_config)
                .await
                .map_err(AccountUsageFetchError::new)
        }
        result => result.map_err(AccountUsageFetchError::new),
    }
}

async fn fetch_with_auth(
    auth: &CodexAuth,
    chatgpt_base_url: &str,
    auth_route_config: &AuthRouteConfig,
) -> Result<AccountUsage> {
    let response = BackendClient::from_auth(
        chatgpt_base_url,
        auth,
        auth_route_config.http_client_factory().clone(),
    )
    .get_rate_limits_with_reset_credits()
    .await?;
    Ok(account_usage(&response))
}

fn is_unauthorized(err: &anyhow::Error) -> bool {
    err.chain().any(|source| {
        source
            .downcast_ref::<RequestError>()
            .is_some_and(RequestError::is_unauthorized)
    })
}

pub(crate) fn login_required(err: &anyhow::Error) -> bool {
    err.chain().any(|source| {
        source
            .downcast_ref::<RefreshTokenFailedError>()
            .is_some_and(|error| {
                matches!(
                    error.reason,
                    RefreshTokenFailedReason::Expired
                        | RefreshTokenFailedReason::Exhausted
                        | RefreshTokenFailedReason::Revoked
                )
            })
    })
}

fn account_usage(response: &RateLimitsWithResetCredits) -> AccountUsage {
    let mut usage = response
        .rate_limits
        .iter()
        .find(|snapshot| snapshot.limit_id.as_deref() == Some("codex"))
        .or_else(|| response.rate_limits.first())
        .map(account_usage_from_snapshot)
        .unwrap_or_default();
    usage.available_resets = response
        .rate_limit_reset_credits
        .as_ref()
        .map(|credits| credits.available_count);
    usage
}

fn account_usage_from_snapshot(snapshot: &RateLimitSnapshot) -> AccountUsage {
    let remaining_percent =
        |used_percent: f64| (100.0 - used_percent).clamp(0.0, 100.0).round() as u8;
    let primary = snapshot.primary.as_ref();
    let secondary = snapshot.secondary.as_ref();
    let primary_is_weekly = secondary.is_none()
        && primary
            .and_then(|window| window.window_minutes)
            .is_some_and(|minutes| {
                (MINUTES_PER_WEEK * 95 / 100..=MINUTES_PER_WEEK * 105 / 100).contains(&minutes)
            });
    let (five_hour, weekly) = if primary_is_weekly {
        (None, primary)
    } else {
        (primary, secondary)
    };
    AccountUsage {
        plan_type: snapshot.plan_type,
        available_resets: None,
        primary_window_minutes: five_hour.and_then(|window| window.window_minutes),
        five_hour_reset_at: five_hour.and_then(|window| window.resets_at),
        five_hour_remaining_percent: five_hour.map(|window| remaining_percent(window.used_percent)),
        five_hour_exhausted: five_hour.is_some_and(|window| window.used_percent >= 100.0),
        weekly_reset_at: weekly.and_then(|window| window.resets_at),
        weekly_unused: weekly.map(|window| window.used_percent == 0.0),
        weekly_remaining_percent: weekly.map(|window| remaining_percent(window.used_percent)),
        weekly_exhausted: weekly.is_some_and(|window| window.used_percent >= 100.0),
    }
}

#[cfg(test)]
#[path = "account_usage_tests.rs"]
mod tests;
