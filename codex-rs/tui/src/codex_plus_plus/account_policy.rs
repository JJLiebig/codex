//! Persistence for Codex++ global settings.

use crate::app::App;
use crate::app_server_session::AppServerSession;
use crate::legacy_core::config::Config;
use codex_config::AutoRedeemResets;
use codex_config::ModelCapacityRetryMode;
use codex_config::ToolActivityPresentation;
use codex_config::WeeklyUsageWindowAutoStart;
use codex_config::types::AutomaticAccountSelection;
use toml::Value as TomlValue;

pub(crate) async fn persist_settings(
    app: &mut App,
    app_server: &AppServerSession,
    automatic_account_selection: AutomaticAccountSelection,
    weekly_usage_window_auto_start: Option<WeeklyUsageWindowAutoStart>,
    auto_redeem_resets: Option<AutoRedeemResets>,
    model_capacity_retry_mode: Option<ModelCapacityRetryMode>,
    tool_activity: ToolActivityPresentation,
    disable_unnecessary_updates: bool,
) {
    let automatic = match automatic_account_selection {
        AutomaticAccountSelection::Enabled => "enabled",
        AutomaticAccountSelection::Disabled => "disabled",
    };
    let mut writes = vec![crate::config_update::replace_config_value(
        "automatic_account_selection",
        serde_json::json!(automatic),
    )];
    if let Some(weekly) = weekly_usage_window_auto_start {
        writes.push(crate::config_update::replace_config_value(
            "weekly_usage_window_auto_start",
            serde_json::json!(match weekly {
                WeeklyUsageWindowAutoStart::Enabled => "enabled",
                WeeklyUsageWindowAutoStart::Disabled => "disabled",
            }),
        ));
    }
    let requested_auto_redeem = auto_redeem_resets.map(|settings| {
        (settings.before_expiry_minutes.is_some()
            || settings.weekly_exhausted_min_wait_hours.is_some())
        .then_some(settings)
    });
    if let Some(settings) = requested_auto_redeem {
        writes.push(match settings {
            Some(settings) => crate::config_update::replace_config_value(
                "auto_redeem_resets",
                serde_json::json!(settings),
            ),
            // Off removes the active user-layer table; inherited user settings remain authoritative.
            None => crate::config_update::clear_config_value("auto_redeem_resets"),
        });
    }
    if let Some(capacity) = model_capacity_retry_mode {
        writes.push(crate::config_update::replace_config_value(
            "model_capacity_retry_mode",
            serde_json::json!(match capacity {
                ModelCapacityRetryMode::Bounded => "bounded",
                ModelCapacityRetryMode::Indefinite => "indefinite",
            }),
        ));
    }
    writes.push(crate::config_update::replace_config_value(
        "codex_plus_plus_tool_activity",
        serde_json::json!(match tool_activity {
            ToolActivityPresentation::Full => "full",
            ToolActivityPresentation::Compact => "compact",
        }),
    ));
    writes.push(crate::config_update::replace_config_value(
        "disable_unnecessary_updates",
        serde_json::json!(disable_unnecessary_updates),
    ));
    let write_error = crate::config_update::write_config_batch(app_server.request_handle(), writes)
        .await
        .err();
    if let Some(err) = &write_error {
        tracing::error!(error = %err, "failed to persist Codex++ settings");
    }

    let cwd = app.config.cwd.display().to_string();
    let response =
        match crate::config_update::read_effective_config(app_server.request_handle(), cwd).await {
            Ok(response) => response,
            Err(err) => {
                tracing::error!(error = %err, "failed to verify Codex++ settings");
                let disable_weekly =
                    weekly_usage_window_auto_start == Some(WeeklyUsageWindowAutoStart::Disabled);
                if disable_weekly {
                    app.config.weekly_usage_window_auto_start =
                        WeeklyUsageWindowAutoStart::Disabled;
                }
                if let Some(requested) = requested_auto_redeem {
                    let retained = retained_auto_redeem_settings(
                        crate::codex_plus_plus::auto_redeem_resets_settings(
                            &app.config.config_layer_stack,
                        ),
                        requested,
                    );
                    cache_auto_redeem_settings(&mut app.config, retained);
                }
                if disable_weekly || requested_auto_redeem.is_some() {
                    app.chat_widget
                        .sync_codex_plus_plus_settings_config(&app.config);
                    sync_scheduler(app);
                }
                if let Some(write_err) = write_error {
                    app.chat_widget
                        .codex_plus_plus_settings_persistence_failed(format!(
                            "{write_err}; unable to reconcile effective settings: {err}"
                        ));
                } else {
                    app.chat_widget
                        .codex_plus_plus_settings_verification_failed(err.to_string());
                }
                return;
            }
        };
    let effective_automatic = match response
        .config
        .additional
        .get("automatic_account_selection")
        .and_then(serde_json::Value::as_str)
    {
        Some("disabled") => AutomaticAccountSelection::Disabled,
        _ => AutomaticAccountSelection::Enabled,
    };
    let effective_weekly = match response
        .config
        .additional
        .get("weekly_usage_window_auto_start")
        .and_then(serde_json::Value::as_str)
    {
        Some("disabled") => WeeklyUsageWindowAutoStart::Disabled,
        _ => WeeklyUsageWindowAutoStart::Enabled,
    };
    let effective_capacity = match response
        .config
        .additional
        .get("model_capacity_retry_mode")
        .and_then(serde_json::Value::as_str)
    {
        Some("indefinite") => ModelCapacityRetryMode::Indefinite,
        _ => ModelCapacityRetryMode::Bounded,
    };
    let effective_quiet_updates = response
        .config
        .additional
        .get("disable_unnecessary_updates")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let effective_tool_activity = match response
        .config
        .additional
        .get("codex_plus_plus_tool_activity")
        .and_then(serde_json::Value::as_str)
    {
        Some("compact") => ToolActivityPresentation::Compact,
        _ => ToolActivityPresentation::Full,
    };
    app.config.automatic_account_selection = effective_automatic;
    app.config.weekly_usage_window_auto_start = effective_weekly;
    app.config.model_capacity_retry_mode = effective_capacity;
    app.config.codex_plus_plus_tool_activity = effective_tool_activity;
    app.chat_widget
        .sync_codex_plus_plus_settings_config(&app.config);
    if let Err(err) = app.refresh_in_memory_config_from_disk().await {
        tracing::error!(error = %err, "failed to refresh Codex++ settings");
        let disable_weekly =
            weekly_usage_window_auto_start == Some(WeeklyUsageWindowAutoStart::Disabled);
        if disable_weekly {
            app.config.weekly_usage_window_auto_start = WeeklyUsageWindowAutoStart::Disabled;
        }
        if let Some(requested) = requested_auto_redeem {
            let retained = retained_auto_redeem_settings(
                crate::codex_plus_plus::auto_redeem_resets_settings(&app.config.config_layer_stack),
                requested,
            );
            cache_auto_redeem_settings(&mut app.config, retained);
        }
        if disable_weekly || requested_auto_redeem.is_some() {
            app.chat_widget
                .sync_codex_plus_plus_settings_config(&app.config);
            sync_scheduler(app);
        }
        app.chat_widget
            .codex_plus_plus_settings_verification_failed(err.to_string());
        return;
    }
    app.chat_widget
        .sync_codex_plus_plus_settings_config(&app.config);
    sync_scheduler(app);
    let effective_auto_redeem =
        crate::codex_plus_plus::auto_redeem_resets_settings(&app.config.config_layer_stack);
    // Authoritative readback matching the request is success even if the write RPC was ambiguous.
    if effective_automatic == automatic_account_selection
        && weekly_usage_window_auto_start.is_none_or(|weekly| effective_weekly == weekly)
        && requested_auto_redeem.is_none_or(|requested| effective_auto_redeem == requested)
        && model_capacity_retry_mode.is_none_or(|capacity| effective_capacity == capacity)
        && effective_quiet_updates == disable_unnecessary_updates
        && effective_tool_activity == tool_activity
    {
        app.chat_widget.codex_plus_plus_settings_persisted(
            effective_automatic,
            effective_weekly,
            effective_capacity,
            effective_tool_activity,
        );
    } else {
        app.chat_widget
            .codex_plus_plus_settings_persistence_overridden(
                effective_automatic,
                effective_weekly,
                effective_capacity,
                effective_tool_activity,
            );
    }
}

fn sync_scheduler(app: &App) {
    if let Some(scheduler) = &app.weekly_window_scheduler {
        scheduler.set_settings(
            app.config.weekly_usage_window_auto_start == WeeklyUsageWindowAutoStart::Enabled,
            crate::codex_plus_plus::auto_redeem_resets_settings(&app.config.config_layer_stack),
        );
    }
}

fn cache_auto_redeem_settings(config: &mut Config, settings: Option<AutoRedeemResets>) {
    let config_toml = config.config_layer_stack.get_user_config_file().cloned();
    let active_user_config = config
        .config_layer_stack
        .get_active_user_layer()
        .map(|layer| layer.config.clone());
    let (Some(config_toml), Some(TomlValue::Table(mut table))) = (config_toml, active_user_config)
    else {
        return;
    };
    match settings.and_then(|settings| TomlValue::try_from(settings).ok()) {
        Some(settings) => {
            table.insert("auto_redeem_resets".to_string(), settings);
        }
        None => {
            table.remove("auto_redeem_resets");
        }
    }
    match config
        .config_layer_stack
        .with_user_config(&config_toml, TomlValue::Table(table))
    {
        Ok(config_layer_stack) => config.config_layer_stack = config_layer_stack,
        Err(err) => tracing::warn!(%err, "failed to update cached Codex++ settings"),
    }
}

// Failed verification may remove triggers, but must not enable an unverified request.
fn retained_auto_redeem_settings(
    current: Option<AutoRedeemResets>,
    requested: Option<AutoRedeemResets>,
) -> Option<AutoRedeemResets> {
    let mut current = current?;
    let requested = requested?;
    if requested.before_expiry_minutes.is_none() {
        current.before_expiry_minutes = None;
    }
    if requested.weekly_exhausted_min_wait_hours.is_none() {
        current.weekly_exhausted_min_wait_hours = None;
    }
    Some(current)
}

#[cfg(test)]
#[path = "account_policy_tests.rs"]
mod tests;
