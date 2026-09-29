//! Authoritative native ChatGPT credentials for a caller-owned publication step.

use chrono::DateTime;
use chrono::Utc;
use codex_protocol::auth::AuthMode;
use codex_protocol::config_types::ForcedLoginMethod;
use tokio::sync::SemaphorePermit;

use super::super::AuthDotJson;
use super::super::AuthManager;
use super::super::load_auth_dot_json_with_guard;
use super::imported_account_refresh::ManagedAuthRefreshLocks;
use crate::account::AccountId;
use crate::account::account_id_for_auth;
use crate::account::is_root_account_marker;
use crate::auth::storage::AuthKeyringBackendKind;
use crate::token_data::parse_jwt_expiration;
use codex_config::types::AuthCredentialsStoreMode;

/// Stable identity of the native source; a real root login remains distinct from an import.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeCredentialSource {
    Root(AccountId),
    Imported(AccountId),
}

/// The only auth fields a proxy publisher needs. Intentionally has no secret-printing `Debug`.
pub struct NativeCredential {
    pub source: NativeCredentialSource,
    pub access_token: String,
    pub upstream_account_id: String,
    pub expires_at: DateTime<Utc>,
    pub plan_type: Option<String>,
}

/// Holds native refresh and topology guards until publication finishes. Drop before inference.
pub struct NativeCredentialSnapshot<'a> {
    credentials: Vec<NativeCredential>,
    selected_source: Option<NativeCredentialSource>,
    _locks: ManagedAuthRefreshLocks,
    _current_source_guard: SemaphorePermit<'a>,
}

impl NativeCredentialSnapshot<'_> {
    pub fn credentials(&self) -> &[NativeCredential] {
        &self.credentials
    }

    pub fn selected_source(&self) -> Option<&NativeCredentialSource> {
        self.selected_source.as_ref()
    }
}

impl AuthManager {
    /// Read disk authority and hold its guards while the caller publishes this snapshot.
    pub async fn export_native_credentials(&self) -> std::io::Result<NativeCredentialSnapshot<'_>> {
        if self.has_external_auth() || !self.is_login_method_allowed(ForcedLoginMethod::Chatgpt) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "native ChatGPT credential export is unavailable",
            ));
        }
        let current_source_guard = self
            .refresh_lock
            .acquire()
            .await
            .map_err(std::io::Error::other)?;
        let locks = self.acquire_managed_auth_refresh_locks().await?;
        let accounts = locks.account_profiles()?;
        let allowed_workspaces = self.effective_chatgpt_workspaces();
        let now = Utc::now();
        let mut credentials = Vec::new();

        if let Some(auth) = load_auth_dot_json_with_guard(
            &self.codex_home,
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
            locks.guard_for(&self.codex_home)?,
        )? && !is_root_account_marker(&auth)
            && let Some(credential) =
                credential_from_auth(&auth, now, None, allowed_workspaces.as_deref())
        {
            credentials.push(credential);
        }

        for (account, home) in accounts {
            if !account.enabled || account.login_required {
                continue;
            }
            let auth = load_auth_dot_json_with_guard(
                &home,
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default(),
                locks.guard_for(&home)?,
            )?;
            if let Some(auth) = auth
                && let Some(credential) = credential_from_auth(
                    &auth,
                    now,
                    Some(&account.id),
                    allowed_workspaces.as_deref(),
                )
            {
                credentials.push(credential);
            }
        }

        let selected_source = if let Some(account_id) = self.active_account_id() {
            credentials
                .iter()
                .find(|credential| {
                    credential.source == NativeCredentialSource::Imported(account_id.clone())
                })
                .map(|credential| credential.source.clone())
        } else {
            credentials
                .iter()
                .find(|credential| matches!(credential.source, NativeCredentialSource::Root(_)))
                .map(|credential| credential.source.clone())
        };

        Ok(NativeCredentialSnapshot {
            credentials,
            selected_source,
            _locks: locks,
            _current_source_guard: current_source_guard,
        })
    }
}

fn credential_from_auth(
    auth: &AuthDotJson,
    now: DateTime<Utc>,
    imported_account_id: Option<&AccountId>,
    allowed_workspaces: Option<&[String]>,
) -> Option<NativeCredential> {
    if auth.resolved_mode() != AuthMode::Chatgpt || is_root_account_marker(auth) {
        return None;
    }
    let tokens = auth.tokens.as_ref()?;
    crate::server::ensure_workspace_allowed(allowed_workspaces, &tokens.id_token.raw_jwt).ok()?;
    let source_id = account_id_for_auth(auth).ok()?;
    if imported_account_id.is_some_and(|id| id != &source_id) {
        return None;
    }
    let upstream_account_id = tokens
        .account_id
        .as_deref()
        .filter(|id| !id.is_empty() && id.trim() == *id)?;
    let expires_at = parse_jwt_expiration(&tokens.access_token).ok()??;
    if expires_at <= now {
        return None;
    }
    Some(NativeCredential {
        source: match imported_account_id {
            Some(_) => NativeCredentialSource::Imported(source_id),
            None => NativeCredentialSource::Root(source_id),
        },
        access_token: tokens.access_token.clone(),
        upstream_account_id: upstream_account_id.to_string(),
        expires_at,
        plan_type: tokens.id_token.get_chatgpt_plan_type_raw(),
    })
}

#[cfg(test)]
#[path = "native_credential_export_tests.rs"]
mod tests;
