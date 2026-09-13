use super::*;
use pretty_assertions::assert_eq;

fn account_candidate(id: &str, automation_enabled: bool) -> AccountCandidate {
    AccountCandidate {
        id: serde_json::from_value(id.into()).expect("account id"),
        display_label: format!("{id}@example.com"),
        priority: 0,
        enabled: true,
        automation_enabled,
        usage_limit_resets_at: None,
        blocked: false,
    }
}

fn picker_candidate(id: &str, is_current: bool) -> account_picker::AccountPickerCandidate {
    account_picker::AccountPickerCandidate {
        id: id.to_string(),
        email: format!("{id}@example.com"),
        available_resets: None,
        primary_window_label: "5h".to_string(),
        five_hour_reset: None,
        five_hour_usage_left_percent: Some(100),
        five_hour_exhausted: false,
        weekly_reset: None,
        weekly_usage_left_percent: Some(100),
        weekly_exhausted: false,
        blocked_until: None,
        blocked: false,
        in_use: false,
        is_current,
        is_default: false,
    }
}

#[test]
fn startup_picker_candidates_are_sorted_alphabetically_by_label() {
    let mut charlie = account_candidate("acct_c", /*automation_enabled*/ true);
    charlie.display_label = "Charlie@example.com".to_string();
    let mut alice = account_candidate("acct_a", /*automation_enabled*/ true);
    alice.display_label = "alice@example.com".to_string();
    let mut bob = account_candidate("acct_b", /*automation_enabled*/ true);
    bob.display_label = "Bob@example.com".to_string();
    let mut bob_again = account_candidate("acct_b2", /*automation_enabled*/ true);
    bob_again.display_label = "bob@example.com".to_string();
    let mut candidates = vec![charlie, alice, bob, bob_again];

    sort_candidates_alphabetically(&mut candidates);

    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.id.as_str())
            .collect::<Vec<_>>(),
        ["acct_a", "acct_b", "acct_b2", "acct_c"]
    );
}

#[test]
fn startup_picker_preflight_is_conservative_for_enabled_or_unreadable_accounts() {
    let codex_home = tempfile::tempdir().expect("create Codex home");
    let index_path = codex_home.path().join("accounts/index.json");

    assert!(!may_run_startup_account_picker(codex_home.path()));

    std::fs::create_dir_all(index_path.parent().expect("accounts directory"))
        .expect("create accounts directory");
    std::fs::write(&index_path, "not json").expect("write unreadable account index");
    assert!(may_run_startup_account_picker(codex_home.path()));

    std::fs::write(
        &index_path,
        r#"{"accounts":[{"id":"acct_enabled","label":"enabled@example.com","auth":{"scope":"file","path":"accounts/acct_enabled/auth.json"}}]}"#,
    )
    .expect("write enabled account index");
    assert!(may_run_startup_account_picker(codex_home.path()));

    std::fs::write(
        index_path,
        r#"{"accounts":[{"id":"acct_disabled","label":"disabled@example.com","enabled":false,"auth":{"scope":"file","path":"accounts/acct_disabled/auth.json"}}]}"#,
    )
    .expect("write disabled account index");
    assert!(!may_run_startup_account_picker(codex_home.path()));
}

#[tokio::test]
async fn mode_less_bedrock_access_keys_skip_imported_account_picker() {
    let codex_home = tempfile::tempdir().expect("create Codex home");
    std::fs::write(
        codex_home.path().join("auth.json"),
        r#"{"bedrock_access_keys":{"access_key_id":"access-key-id","secret_access_key":"secret-access-key"}}"#,
    )
    .expect("write mode-less Bedrock access keys");
    let mut config = crate::legacy_core::config::ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await
        .expect("build config");
    config.cli_auth_credentials_store_mode = codex_config::types::AuthCredentialsStoreMode::File;
    config.automatic_account_selection = AutomaticAccountSelection::Enabled;

    assert!(!root_auth_allows_imported_account_picker(
        &config, /*auto_account*/ false
    ));
}

#[test]
fn auto_account_requires_an_eligible_account() {
    assert_eq!(
        continue_without_account(/*auto_account*/ true)
            .expect_err("auto account must fail")
            .to_string(),
        "no eligible account is available for --auto-account"
    );
}

#[test]
fn automatic_default_ignores_automation_disabled_current_account() {
    let current = account_candidate("acct_current", /*automation_enabled*/ false);
    let alternative = account_candidate("acct_alternative", /*automation_enabled*/ true);
    let candidates = vec![current, alternative];
    let picker_candidates = vec![
        picker_candidate("acct_current", /*is_current*/ true),
        picker_candidate("acct_alternative", /*is_current*/ false),
    ];

    assert_eq!(
        automatic_default_index(&candidates, &picker_candidates),
        Some(1)
    );
}

#[test]
fn automatic_default_ignores_automation_disabled_alternative() {
    let disabled = account_candidate("acct_disabled", /*automation_enabled*/ false);
    let enabled = account_candidate("acct_enabled", /*automation_enabled*/ true);
    let candidates = vec![disabled, enabled];
    let picker_candidates = vec![
        picker_candidate("acct_disabled", /*is_current*/ false),
        picker_candidate("acct_enabled", /*is_current*/ false),
    ];

    assert_eq!(
        automatic_default_index(&candidates, &picker_candidates),
        Some(1)
    );
}

#[test]
fn automatic_default_ignores_accounts_without_loaded_usage() {
    let unknown = account_candidate("acct_unknown", /*automation_enabled*/ true);
    let known = account_candidate("acct_known", /*automation_enabled*/ true);
    let mut unknown_picker = picker_candidate("acct_unknown", /*is_current*/ false);
    unknown_picker.five_hour_usage_left_percent = None;
    unknown_picker.weekly_usage_left_percent = None;
    let mut known_picker = picker_candidate("acct_known", /*is_current*/ false);
    known_picker.in_use = true;
    let candidates = vec![unknown, known];
    let picker_candidates = vec![unknown_picker, known_picker];

    assert_eq!(
        automatic_default_index(&candidates, &picker_candidates),
        Some(1)
    );
    assert_eq!(
        automatic_default_index(&candidates[..1], &picker_candidates[..1]),
        None
    );
}

#[test]
fn picker_shows_local_reset_times_and_available_credits() {
    use chrono::TimeZone;

    for (month, expected) in [(1, "Jan 15 00:30"), (7, "Jul 15 00:30")] {
        let timestamp = chrono::Local
            .with_ymd_and_hms(
                /*year*/ 2026, month, /*day*/ 15, /*hour*/ 0, /*min*/ 30,
                /*sec*/ 0,
            )
            .single()
            .unwrap()
            .timestamp();
        let usage = account_usage::AccountUsage {
            available_resets: Some(2),
            five_hour_reset_at: Some(timestamp),
            weekly_reset_at: Some(timestamp),
            ..Default::default()
        };
        let candidate = account_picker_candidate(
            &account_candidate("acct_local", /*automation_enabled*/ true),
            Some(&usage),
            /*in_use*/ false,
            /*is_current*/ false,
        );
        assert_eq!(
            (
                candidate.five_hour_reset.as_deref(),
                candidate.weekly_reset.as_deref(),
                candidate.available_resets
            ),
            (Some(expected), Some(expected), Some(2)),
        );
    }
}
