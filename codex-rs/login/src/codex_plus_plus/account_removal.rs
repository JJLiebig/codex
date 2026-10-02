//! Remove one imported account without logging out other accounts.

use super::*;

impl AccountStore {
    /// Forget an idle imported account and clear root auth only if it names that account.
    pub fn remove(
        &self,
        account_id: &AccountId,
        root_store_mode: AuthCredentialsStoreMode,
        root_keyring_backend_kind: AuthKeyringBackendKind,
    ) -> io::Result<bool> {
        let _topology_lease = self.acquire_account_topology_lease()?;
        let _account_lease = self.try_acquire_lease(account_id)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "Account is in use. Close its Codex sessions and retry.",
            )
        })?;
        let _reset_lease = self
            .try_acquire_reset_mutation_lease(account_id)?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "Account maintenance is in progress. Retry after it finishes.",
                )
            })?;
        let root_guard = AuthRefreshGuard::acquire(&self.codex_home)?;
        let account_home = self.account_home(account_id);
        let account_guard = AuthRefreshGuard::acquire(&account_home)?;
        let _index_guard = self.acquire_index_lock()?;
        let mut index = self.load_index()?;
        if !index
            .accounts
            .iter()
            .any(|account| &account.id == account_id)
        {
            return Ok(false);
        }
        let root_auth = load_auth_dot_json_with_guard(
            &self.codex_home,
            root_store_mode,
            root_keyring_backend_kind,
            &root_guard,
        )?;
        let clear_root = root_auth.as_ref().is_some_and(|auth| {
            is_managed_chatgpt_auth(auth)
                && account_id_for_auth(auth).is_ok_and(|id| &id == account_id)
        });
        let account_auth = load_auth_dot_json_with_guard(
            &account_home,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
            &account_guard,
        )?;
        index.accounts.retain(|account| &account.id != account_id);
        let result = (|| {
            if clear_root {
                crate::auth::logout_with_guard(
                    &self.codex_home,
                    root_store_mode,
                    root_keyring_backend_kind,
                    &root_guard,
                )?;
            }
            crate::auth::logout_with_guard(
                &account_home,
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default(),
                &account_guard,
            )?;
            self.save_index(&index)
        })();
        if let Err(err) = result {
            let mut rollback_errors = Vec::new();
            if clear_root
                && let Some(auth) = root_auth.as_ref()
                && let Err(rollback_err) = save_auth_with_guard(
                    &self.codex_home,
                    auth,
                    root_store_mode,
                    root_keyring_backend_kind,
                    &root_guard,
                )
            {
                rollback_errors.push(rollback_err.to_string());
            }
            if let Err(rollback_err) =
                restore_file_auth(&account_home, account_auth.as_ref(), &account_guard)
            {
                rollback_errors.push(rollback_err.to_string());
            }
            return if rollback_errors.is_empty() {
                Err(err)
            } else {
                Err(io::Error::other(format!(
                    "{err}; failed to restore credentials: {}",
                    rollback_errors.join("; ")
                )))
            };
        }
        Ok(true)
    }
}

#[cfg(test)]
#[path = "account_removal_tests.rs"]
mod tests;
