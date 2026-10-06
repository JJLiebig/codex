use super::super::AuthDotJson;
use super::super::AuthStorageBackend;
use super::super::AutoAuthStorage;
use super::super::storage_telemetry;
use crate::account_lease::AuthRefreshGuard;
use codex_config::types::AuthCredentialsStoreMode;
use codex_otel::auth_storage::Operation;
use codex_otel::auth_storage::Store;
use tracing::warn;

pub(in crate::auth::storage) fn load(
    storage: &AutoAuthStorage,
    guard: &AuthRefreshGuard,
) -> std::io::Result<Option<AuthDotJson>> {
    let mut telemetry = storage_telemetry::telemetry(
        AuthCredentialsStoreMode::Auto,
        storage.keyring_backend_kind,
        Operation::Load,
    );
    if storage.file_authority.is_active()? {
        let result = storage
            .file_authority
            .load_authoritative(&storage.file_storage, guard);
        telemetry.record_load_attempt(Store::File, &result);
        return result;
    }

    let result = storage.keyring_storage.load();
    telemetry.record_load_attempt(
        storage_telemetry::keyring_store(storage.keyring_backend_kind),
        &result,
    );
    match result {
        Ok(Some(auth)) => Ok(Some(auth)),
        Ok(None) => {
            let result = storage.file_storage.load_with_guard(guard);
            telemetry.record_load_attempt(Store::File, &result);
            result
        }
        Err(err) => {
            warn!("failed to load CLI auth from keyring, falling back to file storage: {err}");
            telemetry.record_secure_error(&err);
            let result = storage.file_storage.load_with_guard(guard);
            telemetry.record_load_attempt(Store::File, &result);
            result
        }
    }
}

pub(in crate::auth::storage) fn save(
    storage: &AutoAuthStorage,
    auth: &AuthDotJson,
    guard: &AuthRefreshGuard,
) -> std::io::Result<()> {
    let mut telemetry = storage_telemetry::telemetry(
        AuthCredentialsStoreMode::Auto,
        storage.keyring_backend_kind,
        Operation::Save,
    );
    if storage.file_authority.is_active()? {
        let result = storage
            .file_authority
            .save_if_authoritative(&storage.file_storage, auth, guard)
            .map(|_| ());
        telemetry.record_save_attempt(Store::File, &result);
        return result;
    }

    let result = storage.keyring_storage.save_with_guard(auth, guard);
    telemetry.record_save_attempt(
        storage_telemetry::keyring_store(storage.keyring_backend_kind),
        &result,
    );
    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            warn!("failed to save auth to keyring, falling back to file storage: {err}");
            telemetry.record_secure_error(&err);
            let result = storage
                .file_authority
                .save_fallback(&storage.file_storage, auth, guard);
            telemetry.record_save_attempt(Store::File, &result);
            result
        }
    }
}

pub(in crate::auth::storage) fn delete(
    storage: &AutoAuthStorage,
    guard: &AuthRefreshGuard,
) -> std::io::Result<bool> {
    let mut telemetry = storage_telemetry::telemetry(
        AuthCredentialsStoreMode::Auto,
        storage.keyring_backend_kind,
        Operation::Delete,
    );
    let result = storage.keyring_storage.delete_with_guard(guard);
    telemetry.record_delete_attempt(Store::Multiple, &result);
    result
}
