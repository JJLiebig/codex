//! Single-store mutations for removing an imported account.

use super::super::*;

pub(crate) fn effective_mode(
    home: &Path,
    mode: AuthCredentialsStoreMode,
    backend: AuthKeyringBackendKind,
) -> std::io::Result<AuthCredentialsStoreMode> {
    if mode != AuthCredentialsStoreMode::Auto {
        return Ok(mode);
    }
    if !FileAuthorityMarker::new(home).is_active()?
        && create_keyring_auth_storage(home.to_path_buf(), Arc::new(DefaultKeyringStore), backend)
            .load()
            .is_ok_and(|auth| auth.is_some())
    {
        Ok(AuthCredentialsStoreMode::Keyring)
    } else {
        Ok(AuthCredentialsStoreMode::File)
    }
}

pub(crate) fn delete(
    home: &Path,
    mode: AuthCredentialsStoreMode,
    backend: AuthKeyringBackendKind,
    guard: &AuthRefreshGuard,
) -> std::io::Result<bool> {
    delete_with_store(home, mode, backend, guard, Arc::new(DefaultKeyringStore))
}

fn delete_with_store(
    home: &Path,
    mode: AuthCredentialsStoreMode,
    backend: AuthKeyringBackendKind,
    guard: &AuthRefreshGuard,
    keyring: Arc<dyn KeyringStore>,
) -> std::io::Result<bool> {
    guard.ensure_matches(home)?;
    if mode != AuthCredentialsStoreMode::Keyring {
        return create_auth_storage_with_store(home.to_path_buf(), mode, keyring, backend)
            .delete_with_guard(guard);
    }
    match backend {
        AuthKeyringBackendKind::Direct => {
            DirectKeyringAuthStorage::new(home.to_path_buf(), keyring).delete_keyring()
        }
        AuthKeyringBackendKind::Secrets => {
            SecretsKeyringAuthStorage::new(home.to_path_buf(), keyring)
                .secrets_manager
                .delete(&SecretScope::Global, &CODEX_AUTH_SECRET_NAME)
                .map_err(std::io::Error::other)
        }
    }
}

pub(crate) fn restore(
    home: &Path,
    auth: &AuthDotJson,
    mode: AuthCredentialsStoreMode,
    backend: AuthKeyringBackendKind,
    guard: &AuthRefreshGuard,
) -> std::io::Result<()> {
    restore_with_store(
        home,
        auth,
        mode,
        backend,
        guard,
        Arc::new(DefaultKeyringStore),
    )
}

fn restore_with_store(
    home: &Path,
    auth: &AuthDotJson,
    mode: AuthCredentialsStoreMode,
    backend: AuthKeyringBackendKind,
    guard: &AuthRefreshGuard,
    keyring: Arc<dyn KeyringStore>,
) -> std::io::Result<()> {
    guard.ensure_matches(home)?;
    let storage = create_auth_storage_with_store(home.to_path_buf(), mode, keyring, backend);
    if mode == AuthCredentialsStoreMode::Keyring {
        storage.save_preserving_file(auth)
    } else {
        storage.save_with_guard(auth, guard)
    }
}

#[cfg(test)]
#[path = "account_removal_tests.rs"]
mod tests;
