//! Single-store mutations for removing an imported account.

use super::super::*;

pub(crate) struct RemovalStorage {
    pub(crate) mode: AuthCredentialsStoreMode,
    file_authority_active: bool,
}

pub(crate) fn capture(
    home: &Path,
    mode: AuthCredentialsStoreMode,
    backend: AuthKeyringBackendKind,
) -> std::io::Result<RemovalStorage> {
    let file_authority_active = FileAuthorityMarker::new(home).is_active()?;
    let mode = if mode != AuthCredentialsStoreMode::Auto {
        mode
    } else if !file_authority_active
        && create_keyring_auth_storage(
            home.to_path_buf(),
            Arc::new(DefaultKeyringStore),
            backend,
            AuthCredentialsStoreMode::Keyring,
        )
        .load()
        .is_ok_and(|auth| auth.is_some())
    {
        AuthCredentialsStoreMode::Keyring
    } else {
        AuthCredentialsStoreMode::File
    };
    Ok(RemovalStorage {
        mode,
        file_authority_active,
    })
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
        AuthKeyringBackendKind::Direct => DirectKeyringAuthStorage::new(
            home.to_path_buf(),
            keyring,
            AuthCredentialsStoreMode::Keyring,
        )
        .delete_keyring(),
        AuthKeyringBackendKind::Secrets => SecretsKeyringAuthStorage::new(
            home.to_path_buf(),
            keyring,
            AuthCredentialsStoreMode::Keyring,
        )
        .secrets_manager
        .delete(&SecretScope::Global, &CODEX_AUTH_SECRET_NAME)
        .map_err(std::io::Error::other),
    }
}

pub(crate) fn restore(
    home: &Path,
    auth: &AuthDotJson,
    snapshot: &RemovalStorage,
    backend: AuthKeyringBackendKind,
    guard: &AuthRefreshGuard,
) -> std::io::Result<()> {
    restore_with_store(
        home,
        auth,
        snapshot,
        backend,
        guard,
        Arc::new(DefaultKeyringStore),
    )
}

fn restore_with_store(
    home: &Path,
    auth: &AuthDotJson,
    snapshot: &RemovalStorage,
    backend: AuthKeyringBackendKind,
    guard: &AuthRefreshGuard,
    keyring: Arc<dyn KeyringStore>,
) -> std::io::Result<()> {
    guard.ensure_matches(home)?;
    let storage =
        create_auth_storage_with_store(home.to_path_buf(), snapshot.mode, keyring, backend);
    if snapshot.mode == AuthCredentialsStoreMode::Keyring {
        storage.save_preserving_file(auth)?;
    } else {
        storage.save_with_guard(auth, guard)?;
    }
    if snapshot.file_authority_active {
        FileAuthorityMarker::new(home).activate()?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "account_removal_tests.rs"]
mod tests;
