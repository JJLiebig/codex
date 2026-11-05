use super::*;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn test_store() -> (TempDir, AccountStore, AccountId) {
    let home = TempDir::new().expect("tempdir");
    let store = AccountStore::new(home.path().into());
    (home, store, AccountId("acct_test".into()))
}

fn external_auth(workspace_id: &str) -> CodexAuth {
    CodexAuth::from_external_chatgpt_tokens(
        "e30.e30.c2ln",
        workspace_id,
        /*chatgpt_plan_type*/ None,
    )
    .unwrap()
}

fn redeeming(phase: &ResetAttemptPhase) -> (&str, &str) {
    let ResetAttemptPhase::Redeeming {
        credit_id,
        redeem_request_id,
    } = phase
    else {
        panic!("expected redeeming phase");
    };
    (credit_id, redeem_request_id)
}

#[test]
fn reset_mutation_lease_excludes_and_releases() {
    let (_home, store, account_id) = test_store();
    let lease = store
        .try_acquire_reset_mutation_lease(&account_id)
        .unwrap()
        .unwrap();
    assert!(
        store
            .try_acquire_reset_mutation_lease(&account_id)
            .unwrap()
            .is_none()
    );
    drop(lease);
    assert!(
        store
            .try_acquire_reset_mutation_lease(&account_id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn ambiguous_attempt_replays_and_definite_noop_gets_a_fresh_uuid() {
    let (_home, store, account_id) = test_store();
    let mut lease = store.acquire_reset_mutation_lease(&account_id).unwrap();
    let first = lease.load_or_begin("credit-a").unwrap();
    let (first_credit, first_request) = redeeming(&first);
    assert_eq!(first_credit, "credit-a");
    drop(lease);
    let mut lease = store.acquire_reset_mutation_lease(&account_id).unwrap();
    assert_eq!(lease.load_or_begin("credit-b").unwrap(), first);
    assert!(!lease.clear_redeeming("wrong-request").unwrap());
    assert!(lease.clear_redeeming(first_request).unwrap());
    let fresh = lease.load_or_begin("credit-a").unwrap();
    assert_ne!(redeeming(&fresh).1, first_request);
}

#[test]
fn confirmed_redemption_recovers_weekly_activation_and_completion() {
    let (_home, store, account_id) = test_store();
    let mut lease = store.acquire_reset_mutation_lease(&account_id).unwrap();
    let attempt = lease.load_or_begin("credit-a").unwrap();
    let request_id = redeeming(&attempt).1.to_string();
    let completed_at = 100;
    let confirmed = lease
        .confirm_redeemed("wrong-request", completed_at)
        .unwrap();
    assert!(!confirmed);
    assert!(lease.confirm_redeemed(&request_id, completed_at).unwrap());
    let expected = ResetState {
        phase: Some(ResetAttemptPhase::ActivatingWeekly),
        completion: Some(ResetCompletion {
            id: request_id,
            completed_at: 100,
            manual: false,
        }),
    };
    assert_eq!(lease.state().unwrap(), expected);
    drop(lease);
    let mut lease = store.acquire_reset_mutation_lease(&account_id).unwrap();
    assert_eq!(lease.state().unwrap(), expected);
    assert!(lease.finish_weekly_activation().unwrap());
    assert_eq!(
        lease.state().unwrap(),
        ResetState {
            phase: None,
            completion: expected.completion,
        }
    );
}

#[test]
fn bad_or_newer_state_fails_closed_without_replacement() {
    let (home, store, account_id) = test_store();
    let mut lease = store.acquire_reset_mutation_lease(&account_id).unwrap();
    let path = home.path().join("accounts/acct_test").join(STATE_FILE);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    for invalid in [
        b"{broken".to_vec(),
        br#"{"version":2,"phase":null,"completion":null}"#.to_vec(),
        vec![b'x'; MAX_STATE_BYTES as usize + 1],
    ] {
        std::fs::write(&path, &invalid).unwrap();
        assert_eq!(
            lease.state().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            lease.load_or_begin("credit-a").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(std::fs::read(&path).unwrap(), invalid);
    }
}

#[tokio::test]
async fn auth_lease_uses_exact_identity_and_has_a_deadline() {
    let (home, store, _account_id) = test_store();
    let auth = external_auth("workspace-a");
    let tokens = auth.get_token_data().unwrap();
    let account_id = super::super::account_id_for_token_data(&tokens).unwrap();
    let held = store.acquire_reset_mutation_lease(&account_id).unwrap();
    let other = external_auth("workspace-b");
    assert!(
        store
            .acquire_reset_mutation_lease_for_auth(&other, Instant::now() + Duration::from_secs(1),)
            .await
            .unwrap()
            .is_some()
    );
    let error = match store
        .acquire_reset_mutation_lease_for_auth(&auth, Instant::now() + Duration::from_millis(10))
        .await
    {
        Ok(_) => panic!("expected lease timeout"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    let (tx, rx) = std::sync::mpsc::channel();
    let import_store = store.clone();
    let import_home = home.path().to_path_buf();
    std::thread::spawn(move || {
        let profile = super::super::tests::import_test_account(
            &import_store,
            &import_home,
            "workspace a",
            "workspace-a",
        );
        tx.send(profile).unwrap();
    });
    assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
    drop(held);
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(1)).unwrap().id,
        account_id
    );
    let error = match store
        .acquire_reset_mutation_lease_for_auth(&auth, Instant::now())
        .await
    {
        Ok(_) => panic!("expected expired deadline"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(
        store
            .acquire_reset_mutation_lease_for_auth(&auth, Instant::now() + Duration::from_secs(1),)
            .await
            .unwrap()
            .is_some()
    );
}

#[test]
fn old_manual_attempt_and_completed_receipt_survive_without_repeating_redemption() {
    let (home, store, account_id) = test_store();
    let path = home.path().join("accounts/acct_test").join(STATE_FILE);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let old = br#"{"version":1,"phase":{"type":"manual_redeeming","redeem_request_id":"original","credit_id":"credit-a","source":"root"},"completion":{"id":"completed","completed_at":100,"manual":true,"source":"imported","reconciliation":"dispatched_unknown"}}"#;
    std::fs::write(&path, old).unwrap();
    let mut lease = store.acquire_reset_mutation_lease(&account_id).unwrap();
    assert_eq!(
        lease.load_or_begin("new-credit").unwrap(),
        ResetAttemptPhase::ManualRedeeming {
            redeem_request_id: "original".into(),
            credit_id: Some("credit-a".into())
        }
    );
    assert_eq!(
        lease.state().unwrap().completion,
        Some(ResetCompletion {
            id: "completed".into(),
            completed_at: 100,
            manual: true
        })
    );
    assert_eq!(std::fs::read(path).unwrap(), old);
}
