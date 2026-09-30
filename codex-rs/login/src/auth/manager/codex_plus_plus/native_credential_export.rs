//! Authoritative native ChatGPT credentials for a caller-owned publication step.

use chrono::DateTime;
use chrono::Utc;
use codex_protocol::auth::AuthMode;
use codex_protocol::config_types::ForcedLoginMethod;
use sha2::Digest;
use sha2::Sha256;
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
    identity: [u8; 32],
}

/// Opaque disk identity captured while publication holds the native source guards.
#[derive(Clone)]
pub struct NativeCredentialExpectation {
    source: NativeCredentialSource,
    pub(super) revision: u64,
    identity: [u8; 32],
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum NativeRequestAdmission {
    Eligible,
    AfterTerminalRefresh,
}

impl NativeCredentialExpectation {
    pub fn source(&self) -> &NativeCredentialSource {
        &self.source
    }
}

impl std::fmt::Debug for NativeCredentialExpectation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeCredentialExpectation")
            .finish_non_exhaustive()
    }
}

/// Holds native refresh and topology guards until publication finishes. Drop before inference.
pub struct NativeCredentialSnapshot<'a> {
    credentials: Vec<NativeCredential>,
    selected_source: Option<NativeCredentialSource>,
    expectation: Option<NativeCredentialExpectation>,
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

    pub fn selected_expectation(&self) -> Option<NativeCredentialExpectation> {
        self.expectation.clone()
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
        self.native_credential_snapshot().await
    }

    /// Hold disk authority for deletion only, even when ChatGPT publication is prohibited.
    /// Prohibited credentials are never eligible to retain a published proxy copy.
    pub async fn native_credential_cleanup_snapshot(
        &self,
    ) -> std::io::Result<NativeCredentialSnapshot<'_>> {
        let mut snapshot = self.native_credential_snapshot().await?;
        if self.has_external_auth() || !self.is_login_method_allowed(ForcedLoginMethod::Chatgpt) {
            snapshot.credentials.clear();
            snapshot.selected_source = None;
            snapshot.expectation = None;
        }
        Ok(snapshot)
    }

    async fn native_credential_snapshot(&self) -> std::io::Result<NativeCredentialSnapshot<'_>> {
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

        let expectation = credentials
            .iter()
            .find(|credential| Some(&credential.source) == selected_source.as_ref())
            .map(|credential| NativeCredentialExpectation {
                source: credential.source.clone(),
                revision: *self.auth_change_receiver().borrow(),
                identity: credential.identity,
            });
        Ok(NativeCredentialSnapshot {
            credentials,
            selected_source,
            expectation,
            _locks: locks,
            _current_source_guard: current_source_guard,
        })
    }

    pub(super) async fn native_request_guard(
        &self,
        expected: &NativeCredentialExpectation,
        admission: NativeRequestAdmission,
    ) -> std::io::Result<Option<crate::account_lease::AuthRefreshGuard>> {
        let guard = self
            .acquire_refresh_file_lock()
            .await
            .map_err(std::io::Error::other)?;
        let Some(guard) = guard else {
            return Ok(None);
        };
        Ok(self
            .native_request_matches(expected, admission, &guard)?
            .then_some(guard))
    }

    pub(super) fn native_request_matches(
        &self,
        expected: &NativeCredentialExpectation,
        admission: NativeRequestAdmission,
        guard: &crate::account_lease::AuthRefreshGuard,
    ) -> std::io::Result<bool> {
        // The caller holds the selector semaphore. Recheck after the asynchronous file lock.
        if self.has_external_auth()
            || !self.is_login_method_allowed(ForcedLoginMethod::Chatgpt)
            || *self.auth_change_receiver().borrow() != expected.revision
            || match &expected.source {
                NativeCredentialSource::Root(_) => self.active_account_id().is_some(),
                NativeCredentialSource::Imported(id) => {
                    self.active_account_id().as_ref() != Some(id)
                }
            }
        {
            return Ok(false);
        }
        if let NativeCredentialSource::Imported(id) = &expected.source
            && !crate::account::AccountStore::new(self.codex_home.clone())
                .list()?
                .iter()
                .any(|account| {
                    &account.id == id
                        && account.enabled
                        && (admission == NativeRequestAdmission::AfterTerminalRefresh
                            || !account.login_required)
                })
        {
            return Ok(false);
        }
        let auth = load_auth_dot_json_with_guard(
            &self.active_auth_home(),
            self.active_auth_credentials_store_mode(),
            self.active_keyring_backend_kind(),
            guard,
        )?;
        let Some(auth) = auth else { return Ok(false) };
        let Some(tokens) = auth.tokens.as_ref() else {
            return Ok(false);
        };
        Ok(crate::server::ensure_workspace_allowed(
            self.effective_chatgpt_workspaces().as_deref(),
            &tokens.id_token.raw_jwt,
        )
        .is_ok()
            && <[u8; 32]>::from(Sha256::digest(serde_json::to_vec(&auth)?)) == expected.identity)
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
        identity: Sha256::digest(serde_json::to_vec(auth).ok()?).into(),
    })
}

#[cfg(test)]
#[path = "native_credential_export_tests.rs"]
mod tests;
