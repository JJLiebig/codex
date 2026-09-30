use super::super::super::cli_proxy_reset::reconcile_cli_proxy_reset;
use super::*;
use codex_login::AccountId;
use codex_login::ResetCompletion;
use codex_login::ResetCredentialSource;
use codex_login::ResetReconciliation;
use codex_protocol::protocol::RateLimitSnapshot;
use pretty_assertions::assert_eq;
use sha2::Digest;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn completion(
    home: &Path,
    source: ResetCredentialSource,
    request: &str,
) -> (AccountId, ResetCompletion) {
    let digest = sha2::Sha256::digest(b"account:workspace-a");
    let id: AccountId = serde_json::from_value(json!(format!("acct_{digest:.16x}"))).unwrap();
    let mut lease = AccountStore::new(home.into())
        .acquire_reset_mutation_lease(&id)
        .unwrap();
    lease
        .begin_manual(request, source, /*credit_id*/ None)
        .unwrap();
    lease.confirm_manual(request, /*completed_at*/ 1).unwrap();
    (id, lease.state().unwrap().completion.unwrap())
}

fn receipt(store: &AccountStore, id: &AccountId) -> ResetCompletion {
    store
        .acquire_reset_mutation_lease(id)
        .unwrap()
        .state()
        .unwrap()
        .completion
        .unwrap()
}

fn healthy() -> Vec<RateLimitSnapshot> {
    vec![
        serde_json::from_value(json!({
            "limit_id": "codex", "secondary": {"used_percent": 1, "window_minutes": 10080}
        }))
        .unwrap(),
    ]
}

async fn reconcile(home: &Path, id: &AccountId, receipt: &ResetCompletion) -> io::Result<()> {
    reconcile_cli_proxy_reset(
        home,
        factory(),
        id,
        receipt,
        Some("workspace-a"),
        &healthy(),
        Some(true),
    )
    .await
}

#[tokio::test]
async fn exact_reset_dispatches_once_and_unknown_requires_positive_current_readback() {
    let fixture = Fixture::new(Duration::ZERO).await;
    fixture.server.reset().await;
    let (id, pending) = completion(
        fixture.home.path(),
        ResetCredentialSource::Root,
        "root-reset",
    );
    let root_name = native_route(&NativeCredentialSource::Root(id.clone())).0;
    let imported_name = native_route(&NativeCredentialSource::Imported(id.clone())).0;
    let entries = Arc::new(Mutex::new(json!([
        {"name": root_name, "auth_index": "root-before-restart", "type": "codex", "disabled": false, "unavailable": true, "cooldowns": [{"reason": "quota"}]},
        {"name": imported_name, "auth_index": "imported", "type": "codex", "disabled": false, "unavailable": true, "cooldowns": [{"reason": "quota"}]},
        {"name": "other.json", "auth_index": "other", "type": "codex", "disabled": false},
        {"name": "claude.json", "auth_index": "claude", "type": "claude", "disabled": false}
    ])));
    let inventory = entries.clone();
    Mock::given(method("GET"))
        .and(path("/v0/management/auth-files"))
        .and(header(
            "authorization",
            format!("Bearer {}", "b".repeat(64)),
        ))
        .respond_with(move |_: &Request| {
            ResponseTemplate::new(200)
                .insert_header("X-CPA-VERSION", "7.3.14")
                .set_body_json(json!({"files": *inventory.lock().unwrap()}))
        })
        .mount(&fixture.server)
        .await;
    let posts = Arc::new(Mutex::new(Vec::new()));
    let observed = posts.clone();
    let state_path = fixture
        .home
        .path()
        .join("accounts")
        .join(id.as_str())
        .join("rate-limit-reset-state.json");
    Mock::given(method("POST"))
        .and(path("/v0/management/reset-quota"))
        .and(header(
            "authorization",
            format!("Bearer {}", "b".repeat(64)),
        ))
        .respond_with(move |request: &Request| {
            // Durable Unknown must precede dispatch, even if the server mutates then errors.
            let state: Value =
                serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
            assert_eq!(state["completion"]["reconciliation"], "dispatched_unknown");
            let index = serde_json::from_slice::<Value>(&request.body).unwrap()["auth_index"]
                .as_str()
                .unwrap()
                .to_owned();
            observed.lock().unwrap().push(index.clone());
            ResponseTemplate::new(if index == "imported" { 500 } else { 200 })
                .set_body_json(json!({"status": "ok", "auth_index": index}))
        })
        .mount(&fixture.server)
        .await;
    // Wrong native usage and missing explicit permission cannot authorize mutation.
    for (account, allowed) in [(Some("other"), Some(true)), (Some("workspace-a"), None)] {
        reconcile_cli_proxy_reset(
            fixture.home.path(),
            factory(),
            &id,
            &pending,
            account,
            &healthy(),
            allowed,
        )
        .await
        .unwrap();
    }
    assert!(posts.lock().unwrap().is_empty());
    // Pre-dispatch retry resolves the current index; it is never persisted as identity.
    entries.lock().unwrap()[0]["auth_index"] = json!("root-after-restart");
    reconcile(fixture.home.path(), &id, &pending).await.unwrap();
    let store = AccountStore::new(fixture.home.path().into());
    let acknowledged = receipt(&store, &id);
    assert_eq!(
        acknowledged.reconciliation,
        ResetReconciliation::Acknowledged
    );
    reconcile(fixture.home.path(), &id, &acknowledged)
        .await
        .unwrap();
    let (_, imported) = completion(
        fixture.home.path(),
        ResetCredentialSource::Imported,
        "imported-reset",
    );
    assert!(
        reconcile(fixture.home.path(), &id, &imported)
            .await
            .is_err()
    );
    let unknown = receipt(&store, &id);
    assert_eq!(
        unknown.reconciliation,
        ResetReconciliation::DispatchedUnknown
    );
    // Reopen after an ambiguous response; a later cooldown cannot be cleared by replay.
    reconcile(fixture.home.path(), &id, &unknown).await.unwrap();
    let mut clear = entries.lock().unwrap()[1].clone();
    clear["cooldowns"] = json!([]);
    clear["unavailable"] = json!(false);
    for patch in [
        json!({"cooldowns": null}),
        json!({"disabled": true}),
        json!({"unavailable": true}),
        json!({"auth_index": "other"}),
        json!({"provider": "claude"}),
    ] {
        let mut entry = clear.clone();
        entry
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        entries.lock().unwrap()[1] = entry;
        reconcile(fixture.home.path(), &id, &unknown).await.unwrap();
        assert_eq!(receipt(&store, &id), unknown);
    }
    entries.lock().unwrap()[1] = clear;
    reconcile(fixture.home.path(), &id, &unknown).await.unwrap();
    assert_eq!(
        receipt(&store, &id).reconciliation,
        ResetReconciliation::ObservedClear
    );
    assert_eq!(
        *posts.lock().unwrap(),
        vec!["root-after-restart", "imported"]
    );
}

#[tokio::test]
async fn reset_without_owned_runtime_remains_pending_without_starting_it() {
    let home = tempfile::tempdir().unwrap();
    let (id, pending) = completion(home.path(), ResetCredentialSource::Root, "pending");
    reconcile(home.path(), &id, &pending).await.unwrap();
    assert!(!home.path().join("cli-proxy").exists());
    assert_eq!(
        AccountStore::new(home.path().into())
            .pending_proxy_resets()
            .unwrap(),
        vec![(id, pending)]
    );
}
