//! Completion-scoped cooldown reconciliation; ambiguous mutations are never replayed.

use std::io;
use std::path::Path;
use std::time::Duration;

use codex_http_client::HttpClientFactory;
use codex_http_client::HttpTransport;
use codex_http_client::Request;
use codex_http_client::ReqwestTransport;
use codex_login::AccountId;
use codex_login::AccountStore;
use codex_login::NativeCredentialSource;
use codex_login::ResetCompletion;
use codex_login::ResetCredentialSource;
use codex_login::ResetReconciliation;
use codex_protocol::protocol::RateLimitSnapshot;
use http::Method;
use serde::Deserialize;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;

use super::cli_proxy_credentials::native_route;
use super::cli_proxy_inventory::read_json;
use super::cli_proxy_runtime::CliProxyRuntime;

/// Call only with a fresh native usage response for this confirmed completion.
/// The account lease must be dropped before calling: runtime attachment precedes it.
pub async fn reconcile_cli_proxy_reset(
    home: &Path,
    factory: HttpClientFactory,
    account_id: &AccountId,
    completion: &ResetCompletion,
    usage_account_id: Option<&str>,
    limits: &[RateLimitSnapshot],
    ordinary_usage_allowed: Option<bool>,
) -> io::Result<()> {
    let Some(source) = completion.source else {
        return Ok(());
    };
    if !completion.needs_proxy_reconciliation() {
        return Ok(());
    }
    let Some(backend_id) = usage_account_id else {
        return Ok(());
    };
    let digest = Sha256::digest(format!("account:{backend_id}"));
    let mut exact = limits
        .iter()
        .filter(|limit| limit.limit_id.as_deref() == Some("codex"));
    let Some(limit) = exact.next() else {
        return Ok(());
    };
    let windows: Vec<_> = [limit.primary.as_ref(), limit.secondary.as_ref()]
        .into_iter()
        .flatten()
        .collect();
    if ordinary_usage_allowed != Some(true)
        || format!("acct_{digest:x}").get(..21) != Some(account_id.as_str())
        || exact.next().is_some()
        || limit.rate_limit_reached_type.is_some()
        || limit.spend_control_reached == Some(true)
        || windows
            .iter()
            .filter(|window| window.window_minutes == Some(7 * 24 * 60))
            .count()
            != 1
        || !windows.iter().all(|window| {
            window.used_percent.is_finite() && (0.0..100.0).contains(&window.used_percent)
        })
    {
        return Ok(());
    }
    let runtime = CliProxyRuntime::new(home.to_path_buf(), /*executable*/ None);
    let Some(attached) = runtime.attach_only(factory.clone()).await? else {
        return Ok(());
    };
    let Some(endpoint) = &attached.endpoint else {
        return Ok(());
    };
    let store = AccountStore::new(home.to_path_buf());
    let Some(mut lease) = store.try_acquire_reset_mutation_lease(account_id)? else {
        return Ok(());
    };
    let state = lease.state()?;
    if state.completion.as_ref() != Some(completion)
        || !matches!(
            state.phase,
            None | Some(codex_login::ResetAttemptPhase::ActivatingWeekly)
        )
    {
        return Ok(());
    }
    let source = match source {
        ResetCredentialSource::Root => NativeCredentialSource::Root(account_id.clone()),
        ResetCredentialSource::Imported => NativeCredentialSource::Imported(account_id.clone()),
    };
    let name = native_route(&source).0;
    #[derive(Deserialize)]
    struct Files {
        files: Vec<File>,
    }
    #[derive(Deserialize)]
    struct File {
        name: String,
        auth_index: Option<String>,
        provider: Option<String>,
        #[serde(rename = "type")]
        kind: Option<String>,
        disabled: Option<bool>,
        unavailable: Option<bool>,
        cooldowns: Option<Vec<Value>>,
    }
    let url = url::Url::parse(&endpoint.base_url)
        .and_then(|url| url.join("/v0/management/auth-files"))
        .map_err(io::Error::other)?;
    let transport = ReqwestTransport::from_http_client(runtime.http_client(&factory)?);
    let mut remaining = 1024 * 1024;
    let files: Files = read_json(&transport, url.clone(), endpoint, &mut remaining).await?;
    let mut entries = files.files.iter().filter(|file| file.name == name);
    let Some(file) = entries.next() else {
        return Ok(());
    };
    let Some(index) = file.auth_index.as_deref().filter(|index| !index.is_empty()) else {
        return Ok(());
    };
    if entries.next().is_some()
        || file.disabled != Some(false)
        || file.kind.as_deref().or(file.provider.as_deref()) != Some("codex")
        || file
            .provider
            .as_deref()
            .is_some_and(|provider| provider != "codex")
        || files
            .files
            .iter()
            .filter(|file| file.auth_index.as_deref() == Some(index))
            .count()
            != 1
    {
        return Ok(());
    }
    let Some(cooldowns) = &file.cooldowns else {
        return Ok(());
    };
    if cooldowns.is_empty() && file.unavailable == Some(false) {
        lease.reconcile_proxy(completion, ResetReconciliation::ObservedClear)?;
        return Ok(());
    }
    if completion.reconciliation != ResetReconciliation::Pending
        || cooldowns.is_empty()
        || file.unavailable.is_none()
        || !cooldowns.iter().all(|cooldown| {
            matches!(
                cooldown["reason"].as_str(),
                Some("quota" | "credential_quota")
            )
        })
    {
        return Ok(());
    }
    let mut request = Request::new(
        Method::POST,
        url.join("reset-quota").map_err(io::Error::other)?.into(),
    )
    .with_json(&serde_json::json!({"auth_index": index}));
    let mut authorization = format!("Bearer {}", endpoint.management_key)
        .parse::<http::HeaderValue>()
        .map_err(io::Error::other)?;
    authorization.set_sensitive(true);
    request
        .headers
        .insert(http::header::AUTHORIZATION, authorization);
    request.timeout = Some(Duration::from_secs(/*secs*/ 5));
    request.response_body_limit_bytes = Some(4096);
    if !lease.reconcile_proxy(completion, ResetReconciliation::DispatchedUnknown)? {
        return Ok(());
    }
    // Any exit after this write is unknown until acknowledgment or positive readback.
    let response = transport.execute(request).await.map_err(io::Error::other)?;
    let acknowledgment: Value = serde_json::from_slice(&response.body).map_err(io::Error::other)?;
    if acknowledgment["status"] == "ok" && acknowledgment["auth_index"] == index {
        let mut dispatched = completion.clone();
        dispatched.reconciliation = ResetReconciliation::DispatchedUnknown;
        lease.reconcile_proxy(&dispatched, ResetReconciliation::Acknowledged)?;
    }
    Ok(())
}
