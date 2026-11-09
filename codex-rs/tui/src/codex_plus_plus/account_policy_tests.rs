use super::*;
use pretty_assertions::assert_eq;

#[test]
fn failed_verification_applies_opt_outs_without_enabling_triggers() {
    let both = AutoRedeemResets {
        weekly_exhausted_min_wait_hours: std::num::NonZeroU64::new(72),
        ..AutoRedeemResets::default()
    };
    let expiry_only = AutoRedeemResets {
        weekly_exhausted_min_wait_hours: None,
        ..both
    };
    let exhaustion_only = AutoRedeemResets {
        before_expiry_minutes: None,
        ..both
    };
    for (current, requested, expected) in [
        (Some(both), Some(expiry_only), Some(expiry_only)),
        (Some(both), Some(exhaustion_only), Some(exhaustion_only)),
        (Some(expiry_only), Some(both), Some(expiry_only)),
        (None, Some(both), None),
        (Some(both), None, None),
    ] {
        assert_eq!(retained_auto_redeem_settings(current, requested), expected);
    }
}

#[tokio::test]
async fn settings_reload_external_changes_and_expiry_opt_out_survives_save() {
    let home = tempfile::tempdir().unwrap();
    let mut app = crate::app::test_support::make_test_app().await;
    app.config.codex_home = home.path().to_path_buf().try_into().unwrap();
    app.config.sqlite = codex_state::SqliteConfig::new_for_testing(app.config.codex_home.clone());
    let config_path = home.path().join("config.toml");
    std::fs::write(&config_path, "").unwrap();
    assert!(refresh_settings(&mut app).await);
    assert!(
        crate::codex_plus_plus::auto_redeem_resets_settings(&app.config.config_layer_stack)
            .is_some()
    );
    // Another session changes the settings before this session opens its popup.
    std::fs::write(
        &config_path,
        "[auto_redeem_resets]\nweekly_exhausted_min_wait_hours=96",
    )
    .unwrap();
    assert!(refresh_settings(&mut app).await);
    assert_eq!(
        crate::codex_plus_plus::auto_redeem_resets_settings(
            &app.chat_widget.config_ref().config_layer_stack
        ),
        Some(AutoRedeemResets {
            before_expiry_minutes: None,
            weekly_exhausted_min_wait_hours: std::num::NonZeroU64::new(96),
        })
    );
    let server = crate::start_embedded_app_server_for_picker(&app.config)
        .await
        .unwrap();
    let automatic = app.config.automatic_account_selection;
    let activity = app.config.codex_plus_plus_tool_activity;
    persist_settings(
        &mut app,
        &server,
        automatic,
        /*weekly_usage_window_auto_start*/ None,
        Some(AutoRedeemResets {
            before_expiry_minutes: None,
            weekly_exhausted_min_wait_hours: None,
        }),
        /*model_capacity_retry_mode*/ None,
        activity,
        /*disable_unnecessary_updates*/ true,
    )
    .await;
    let config = crate::legacy_core::config::ConfigBuilder::default()
        .codex_home(home.path().into())
        .build()
        .await
        .unwrap();
    assert_eq!(
        crate::codex_plus_plus::auto_redeem_resets_settings(&config.config_layer_stack),
        None
    );
}
