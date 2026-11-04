//! Read-only imported-account usage reporting for the CLI.

use super::AccountUsage;
use super::fetch;
use crate::legacy_core::config::Config;
use anyhow::Context;
use chrono::DateTime;
use chrono::SecondsFormat;
use codex_login::AccountStore;
use futures::future::join_all;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use std::fmt;

/// One usage fetch per imported account, shared by JSON and TOON output.
#[derive(Serialize)]
pub struct AccountUsageReport {
    accounts: Vec<AccountReport>,
}

#[derive(Serialize)]
struct AccountReport {
    id: String,
    label: String,
    primary: Window,
    weekly: Window,
    status: &'static str,
    error: Option<&'static str>,
}

#[derive(Serialize)]
struct Window {
    remaining_percent: Option<u8>,
    resets_at: Option<i64>,
    window_minutes: Option<i64>,
}

/// Fetch usage without selecting accounts or persisting usage and login metadata.
/// Credential loading and refresh retain the account picker's authentication policy.
pub async fn account_usage_report(config: &Config) -> anyhow::Result<AccountUsageReport> {
    let store = AccountStore::new(config.codex_home.to_path_buf());
    let accounts = store.list().context("failed to list accounts")?;
    let accounts = join_all(accounts.iter().map(|account| async {
        let home = config.codex_home.join("accounts").join(account.id.as_str());
        let result = fetch(home.to_path_buf(), config.clone()).await;
        // Backend errors may contain response bodies or credentials; emit only safe categories.
        let (usage, error) = match result {
            Ok(usage) => (usage, None),
            Err(err) => (
                AccountUsage::default(),
                Some(
                    if super::login_required(&err.error) || super::is_unauthorized(&err.error) {
                        "authentication_failed"
                    } else {
                        "usage_unavailable"
                    },
                ),
            ),
        };
        AccountReport {
            id: account.id.to_string(),
            label: account.label.clone(),
            primary: Window {
                remaining_percent: usage.five_hour_remaining_percent,
                resets_at: usage.five_hour_reset_at,
                window_minutes: usage.primary_window_minutes,
            },
            weekly: Window {
                remaining_percent: usage.weekly_remaining_percent,
                resets_at: usage.weekly_reset_at,
                window_minutes: usage.weekly_window_minutes,
            },
            status: if error.is_some() {
                "error"
            } else if usage.five_hour_remaining_percent.is_none()
                && usage.weekly_remaining_percent.is_none()
            {
                "unknown"
            } else {
                "ok"
            },
            error,
        }
    }))
    .await;
    Ok(AccountUsageReport { accounts })
}

impl fmt::Display for AccountUsageReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "accounts[{}]{{id,label,primary_window_minutes,primary_remaining_percent,primary_resets_at,weekly_remaining_percent,weekly_window_minutes,weekly_resets_at,status,error}}:",
            self.accounts.len()
        )?;
        let timestamp = |seconds: Option<i64>| {
            seconds
                .and_then(|seconds| DateTime::from_timestamp(seconds, /*nsecs*/ 0))
                .map(|time| time.to_rfc3339_opts(SecondsFormat::Secs, /*use_z*/ true))
        };
        for account in &self.accounts {
            let values = [
                json!(account.id),
                json!(account.label),
                json!(account.primary.window_minutes),
                json!(account.primary.remaining_percent),
                json!(timestamp(account.primary.resets_at)),
                json!(account.weekly.remaining_percent),
                json!(account.weekly.window_minutes),
                json!(timestamp(account.weekly.resets_at)),
                json!(account.status),
                json!(account.error),
            ];
            writeln!(
                f,
                "  {}",
                values
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "account_usage_report_tests.rs"]
mod tests;
