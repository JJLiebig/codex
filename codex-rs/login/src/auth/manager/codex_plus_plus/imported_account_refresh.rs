use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

use codex_config::types::AuthCredentialsStoreMode;
use codex_config::types::AutomaticAccountSelection;
use codex_protocol::auth::RefreshTokenFailedReason;

use super::super::AuthManager;
use super::super::CodexAuth;
use super::super::RefreshTokenError;
use super::super::RefreshTokenFailedError;
use super::super::ReloadOutcome;
use super::super::load_auth_dot_json_with_guard;
use super::super::logout_all_stores_with_guard;
use super::super::revoke_auth_tokens;
use super::native_credential_export::NativeCredentialExpectation;
use super::native_credential_export::NativeRequestAdmission;
use crate::account::AccountId;
use crate::account::AccountProfile;
use crate::account::AccountStore;
use crate::account::account_id_for_auth;
use crate::account::is_root_account_marker;
use crate::account_lease::AccountLease;
use crate::account_lease::AuthRefreshGuard;
use crate::auth::storage::AuthDotJson;
use crate::auth::storage::AuthKeyringBackendKind;

const IMPORTED_ACCOUNT_LOGIN_REQUIRED_MESSAGE: &str =
    "This account needs you to sign in again. Run `codex account add` to continue.";
const AUTOMATIC_ACCOUNT_SELECTION_DISABLED_MESSAGE: &str = "This account needs you to sign in again. Automatic account selection is disabled; choose another account in the Codex TUI, run `codex account add`, or enable automatic account selection.";

pub(in crate::auth::manager) fn root_auth_is_account_marker(root_auth: Option<&CodexAuth>) -> bool {
    root_auth
        .and_then(CodexAuth::get_current_auth_json)
        .is_some_and(|auth| is_root_account_marker(&auth))
}

pub(in crate::auth::manager) fn root_auth_reauthenticates_login_required_account(
    codex_home: &Path,
    root_auth: Option<&CodexAuth>,
) -> bool {
    root_auth
        .and_then(CodexAuth::get_current_auth_json)
        .filter(|auth| !is_root_account_marker(auth))
        .and_then(|auth| account_id_for_auth(&auth).ok())
        .is_some_and(|root_account_id| {
            AccountStore::new(codex_home.to_path_buf())
                .list()
                .is_ok_and(|accounts| {
                    accounts
                        .iter()
                        .any(|account| account.id == root_account_id && account.login_required)
                })
        })
}

pub(in crate::auth::manager) struct ManagedAuthRefreshLocks {
    account_store: AccountStore,
    account_homes: Vec<PathBuf>,
    index_readable: bool,
    index_guard: Option<AccountLease>,
    refresh_guards: Vec<AuthRefreshGuard>,
    topology_lease: Option<AccountLease>,
    _reset_leases: Vec<crate::ResetMutationLease>,
}

impl ManagedAuthRefreshLocks {
    pub(in crate::auth::manager) fn account_profiles(
        &self,
    ) -> std::io::Result<Vec<(AccountProfile, PathBuf)>> {
        if !self.index_readable {
            return Err(std::io::Error::other("account index is unreadable"));
        }
        self.account_store
            .list()?
            .into_iter()
            .filter_map(|account| {
                let home = self.account_store.account_home(&account.id);
                match std::fs::metadata(home.join("auth.json")) {
                    Ok(metadata) if metadata.is_file() => Some(Ok((account, home))),
                    Ok(_) => None,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                    Err(err) => Some(Err(err)),
                }
            })
            .collect()
    }

    pub(in crate::auth::manager) fn account_homes(&self) -> &[PathBuf] {
        &self.account_homes
    }

    pub(in crate::auth::manager) fn guard_for(
        &self,
        auth_home: &Path,
    ) -> std::io::Result<&AuthRefreshGuard> {
        self.refresh_guards
            .iter()
            .find(|guard| guard.ensure_matches(auth_home).is_ok())
            .ok_or_else(|| std::io::Error::other("auth refresh guard is missing"))
    }

    pub(in crate::auth::manager) fn disable_all(&self) -> std::io::Result<bool> {
        if self.index_guard.is_none() {
            return Err(std::io::Error::other("account index lock is not held"));
        }
        if self.index_readable {
            self.account_store.disable_all_unlocked()
        } else {
            Ok(false)
        }
    }

    pub(in crate::auth::manager) fn release_index_lock(&mut self) {
        self.index_guard = None;
    }

    pub(in crate::auth::manager) async fn reacquire_index_lock(&mut self) -> std::io::Result<()> {
        let account_store = self.account_store.clone();
        self.index_guard = Some(
            tokio::task::spawn_blocking(move || account_store.acquire_index_lock())
                .await
                .map_err(std::io::Error::other)??,
        );
        Ok(())
    }
}

impl AuthManager {
    pub(in crate::auth::manager) async fn acquire_refresh_file_lock(
        &self,
    ) -> Result<Option<AuthRefreshGuard>, RefreshTokenError> {
        if self.has_external_auth() {
            return Ok(None);
        }
        let auth_home = self.active_auth_home();
        tokio::task::spawn_blocking(move || acquire_refresh_file_lock(&auth_home))
            .await
            .map_err(|err| RefreshTokenError::Transient(std::io::Error::other(err)))?
            .map(Some)
            .map_err(RefreshTokenError::Transient)
    }

    pub(in crate::auth::manager) async fn acquire_managed_auth_refresh_locks(
        &self,
    ) -> std::io::Result<ManagedAuthRefreshLocks> {
        let codex_home = self.codex_home.clone();
        tokio::task::spawn_blocking(move || acquire_managed_auth_refresh_locks(&codex_home))
            .await
            .map_err(std::io::Error::other)?
    }

    async fn reacquire_managed_auth_refresh_locks(
        &self,
        topology_lease: AccountLease,
    ) -> std::io::Result<ManagedAuthRefreshLocks> {
        let codex_home = self.codex_home.clone();
        tokio::task::spawn_blocking(move || {
            acquire_managed_auth_refresh_locks_with_topology(&codex_home, topology_lease)
        })
        .await
        .map_err(std::io::Error::other)?
    }

    pub(in crate::auth::manager) async fn revoke_managed_auth(
        &self,
        locks: &ManagedAuthRefreshLocks,
    ) {
        let Ok(root_guard) = locks.guard_for(&self.codex_home) else {
            tracing::warn!("auth refresh guard is missing during revocation");
            return;
        };
        let mut auth_snapshots = Vec::new();
        if let Some(auth) = self
            .auth_cached()
            .and_then(|auth| auth.get_current_auth_json())
            .filter(|auth| !is_root_account_marker(auth))
        {
            auth_snapshots.push(auth);
        }
        if let Some(auth) = load_auth_snapshot(
            &self.codex_home,
            AuthCredentialsStoreMode::Ephemeral,
            AuthKeyringBackendKind::default(),
            root_guard,
        )
        .filter(|auth| !is_root_account_marker(auth))
        {
            auth_snapshots.push(auth);
        }
        if self.auth_credentials_store_mode != AuthCredentialsStoreMode::Ephemeral
            && let Some(auth) = load_auth_snapshot(
                &self.codex_home,
                self.auth_credentials_store_mode,
                self.keyring_backend_kind,
                root_guard,
            )
            .filter(|auth| !is_root_account_marker(auth))
        {
            auth_snapshots.push(auth);
        }
        for account_home in locks.account_homes() {
            let Ok(guard) = locks.guard_for(account_home) else {
                tracing::warn!(auth_home = %account_home.display(), "auth refresh guard is missing during revocation");
                continue;
            };
            if let Some(auth) = load_auth_snapshot(
                account_home,
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default(),
                guard,
            ) {
                auth_snapshots.push(auth);
            }
        }

        let mut revoked_tokens = HashSet::new();
        for auth in auth_snapshots {
            let Some(token) = revocation_token(&auth) else {
                continue;
            };
            if !revoked_tokens.insert(token.to_string()) {
                continue;
            }
            if let Err(err) = revoke_auth_tokens(Some(&auth), &self.auth_route_config).await {
                tracing::warn!("failed to revoke auth tokens during logout: {err}");
            }
        }
    }

    pub(in crate::auth::manager) async fn quiesce_managed_account_resets(
        &self,
        mut auth_locks: ManagedAuthRefreshLocks,
    ) -> std::io::Result<(ManagedAuthRefreshLocks, bool)> {
        let account_homes = auth_locks.account_homes.clone();
        let expected_account_homes = account_homes.clone();
        let disabled = if auth_locks.index_readable {
            auth_locks.account_store.disable_all_unlocked()?
        } else {
            false
        };
        let topology_lease = auth_locks
            .topology_lease
            .take()
            .ok_or_else(|| std::io::Error::other("account topology lease is missing"))?;
        let account_store = auth_locks.account_store.clone();
        drop(auth_locks);
        let reset_leases = tokio::task::spawn_blocking(move || {
            account_homes
                .iter()
                .map(|account_home| account_store.wait_for_reset_mutation_home_idle(account_home))
                .collect::<std::io::Result<Vec<_>>>()
        })
        .await
        .map_err(std::io::Error::other)??;
        let mut auth_locks = self
            .reacquire_managed_auth_refresh_locks(topology_lease)
            .await?;
        if auth_locks.account_homes != expected_account_homes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "account set changed while preparing logout; retry",
            ));
        }
        auth_locks._reset_leases = reset_leases;
        Ok((auth_locks, disabled))
    }

    pub(in crate::auth::manager) async fn recover_terminal_imported_refresh(
        &self,
        result: Result<(), RefreshTokenError>,
        attempted_account_id: Option<AccountId>,
        guard: AuthRefreshGuard,
        expected: Option<&NativeCredentialExpectation>,
    ) -> Result<(), RefreshTokenError> {
        let terminal = matches!(
            result
                .as_ref()
                .err()
                .and_then(RefreshTokenError::failed_reason),
            Some(
                RefreshTokenFailedReason::Expired
                    | RefreshTokenFailedReason::Exhausted
                    | RefreshTokenFailedReason::Revoked
            )
        );
        let Some(attempted_account_id) = terminal.then_some(attempted_account_id).flatten() else {
            return result;
        };
        if let Some(expected) = expected
            && !self
                .native_request_matches(expected, NativeRequestAdmission::Eligible, &guard)
                .map_err(RefreshTokenError::Transient)?
        {
            return Err(native_source_changed());
        }
        if self.active_account_id().as_ref() == Some(&attempted_account_id) {
            let expected_account_id = self
                .auth_cached()
                .as_ref()
                .and_then(CodexAuth::get_account_id);
            if matches!(
                self.reload_if_account_id_matches(expected_account_id.as_deref(), Some(&guard))
                    .await,
                ReloadOutcome::ReloadedChanged
            ) {
                return Ok(());
            }
        }

        AccountStore::new(self.codex_home.clone())
            .record_login_required(&attempted_account_id)
            .map_err(RefreshTokenError::Transient)?;
        drop(guard);
        if self.active_account_id().as_ref() == Some(&attempted_account_id) {
            self.move_off_imported_account_requiring_login(attempted_account_id, expected)
                .await
        } else {
            Ok(())
        }
    }

    pub(in crate::auth::manager) async fn reconcile_imported_account_refresh_readiness(
        &self,
        guard: Option<AuthRefreshGuard>,
    ) -> Result<Option<AuthRefreshGuard>, RefreshTokenError> {
        let Some(active_account_id) = self.active_account_id() else {
            return Ok(guard);
        };
        if self.active_auth_home() == self.codex_home {
            return Ok(guard);
        }
        let login_required = AccountStore::new(self.codex_home.clone())
            .list()
            .map_err(RefreshTokenError::Transient)?
            .into_iter()
            .find(|account| account.id == active_account_id)
            .is_some_and(|account| account.login_required);
        if !login_required {
            return Ok(guard);
        }

        drop(guard);
        self.move_off_imported_account_requiring_login(active_account_id, /*expected*/ None)
            .await?;
        Ok(None)
    }

    pub(in crate::auth::manager) fn logout_all_managed_auth(
        &self,
        auth_locks: &ManagedAuthRefreshLocks,
    ) -> std::io::Result<bool> {
        let mut removed = auth_locks.disable_all()?;
        removed |= logout_all_stores_with_guard(
            &self.codex_home,
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
            auth_locks.guard_for(&self.codex_home)?,
        )?;
        for account_home in auth_locks.account_homes() {
            removed |= logout_all_stores_with_guard(
                account_home,
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default(),
                auth_locks.guard_for(account_home)?,
            )?;
        }
        Ok(removed)
    }

    async fn move_off_imported_account_requiring_login(
        &self,
        active_account_id: AccountId,
        expected: Option<&NativeCredentialExpectation>,
    ) -> Result<(), RefreshTokenError> {
        if self.automatic_account_selection() == AutomaticAccountSelection::Disabled {
            return Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                RefreshTokenFailedReason::Other,
                AUTOMATIC_ACCOUNT_SELECTION_DISABLED_MESSAGE.to_string(),
            )));
        }
        let attempted_account_ids = HashSet::from([active_account_id.to_string()]);
        let outcome = self
            .switch_to_next_imported_account_unlocked(
                &attempted_account_ids,
                expected.map(|expected| (expected, NativeRequestAdmission::AfterTerminalRefresh)),
            )
            .await;
        if outcome
            == super::imported_account_selection::ImportedAccountSwitchOutcome::RequestSourceChanged
        {
            return Err(native_source_changed());
        }
        if outcome != super::imported_account_selection::ImportedAccountSwitchOutcome::NoCandidate {
            tracing::info!(%active_account_id, "switched away from imported account that requires login");
            Ok(())
        } else {
            let _guard = if let Some(expected) = expected {
                Some(
                    self.native_request_guard(
                        expected,
                        NativeRequestAdmission::AfterTerminalRefresh,
                    )
                    .await
                    .map_err(RefreshTokenError::Transient)?
                    .ok_or_else(native_source_changed)?,
                )
            } else {
                None
            };
            self.clear_active_imported_account();
            self.set_cached_auth(/*new_auth*/ None);
            Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                RefreshTokenFailedReason::Other,
                IMPORTED_ACCOUNT_LOGIN_REQUIRED_MESSAGE.to_string(),
            )))
        }
    }
}

pub(super) fn native_source_changed() -> RefreshTokenError {
    RefreshTokenError::Permanent(RefreshTokenFailedError::new(
        RefreshTokenFailedReason::Other,
        "The account changed while the request was running. Try again.",
    ))
}

fn acquire_refresh_file_lock(auth_home: &Path) -> std::io::Result<AuthRefreshGuard> {
    AuthRefreshGuard::acquire(auth_home)
}

fn acquire_managed_auth_refresh_locks(
    codex_home: &Path,
) -> std::io::Result<ManagedAuthRefreshLocks> {
    let topology_lease =
        AccountStore::new(codex_home.to_path_buf()).acquire_account_topology_lease()?;
    acquire_managed_auth_refresh_locks_with_topology(codex_home, topology_lease)
}

fn acquire_managed_auth_refresh_locks_with_topology(
    codex_home: &Path,
    topology_lease: AccountLease,
) -> std::io::Result<ManagedAuthRefreshLocks> {
    loop {
        let account_store = AccountStore::new(codex_home.to_path_buf());
        let (account_homes, index_readable) = file_account_homes(&account_store)?;
        let mut auth_homes = vec![codex_home.to_path_buf()];
        auth_homes.extend(account_homes.iter().cloned());
        auth_homes.sort();
        auth_homes.dedup();
        let refresh_guards = auth_homes
            .iter()
            .map(|auth_home| acquire_refresh_file_lock(auth_home))
            .collect::<std::io::Result<Vec<_>>>()?;
        let index_guard = account_store.acquire_index_lock()?;
        let (current_account_homes, current_index_readable) = file_account_homes(&account_store)?;
        if current_account_homes == account_homes && current_index_readable == index_readable {
            return Ok(ManagedAuthRefreshLocks {
                account_store,
                account_homes,
                index_readable,
                index_guard: Some(index_guard),
                refresh_guards,
                topology_lease: Some(topology_lease),
                _reset_leases: Vec::new(),
            });
        }
    }
}

fn file_account_homes(account_store: &AccountStore) -> std::io::Result<(Vec<PathBuf>, bool)> {
    let (mut account_homes, index_readable) = match account_store.file_account_profiles() {
        Ok(profiles) => (
            profiles
                .into_iter()
                .map(|(_account, account_home)| account_home)
                .collect(),
            true,
        ),
        Err(err) => {
            tracing::warn!(%err, "account index is unreadable during logout; scanning account auth files");
            (account_store.file_auth_homes()?, false)
        }
    };
    account_homes.sort();
    account_homes.dedup();
    Ok((account_homes, index_readable))
}

fn load_auth_snapshot(
    auth_home: &Path,
    store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
    guard: &AuthRefreshGuard,
) -> Option<AuthDotJson> {
    match load_auth_dot_json_with_guard(auth_home, store_mode, keyring_backend_kind, guard) {
        Ok(auth) => auth,
        Err(err) => {
            tracing::warn!(
                auth_home = %auth_home.display(),
                "failed to load stored auth during logout: {err}"
            );
            None
        }
    }
}

fn revocation_token(auth: &AuthDotJson) -> Option<&str> {
    let tokens = auth.tokens.as_ref()?;
    if !tokens.refresh_token.is_empty() {
        Some(tokens.refresh_token.as_str())
    } else if !tokens.access_token.is_empty() {
        Some(tokens.access_token.as_str())
    } else {
        None
    }
}
