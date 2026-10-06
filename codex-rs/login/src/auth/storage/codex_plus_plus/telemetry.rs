use super::super::delete_file_if_exists;
use super::super::storage_telemetry;
use codex_config::types::AuthCredentialsStoreMode;
use codex_config::types::AuthKeyringBackendKind;
use codex_otel::auth_storage::Operation;
use codex_otel::auth_storage::Store;
use std::path::Path;

pub(in crate::auth::storage) fn cleanup_fallback(
    home: &Path,
    mode: AuthCredentialsStoreMode,
    kind: AuthKeyringBackendKind,
) -> std::io::Result<bool> {
    let mut telemetry = storage_telemetry::telemetry(mode, kind, Operation::Cleanup);
    let result = delete_file_if_exists(home);
    telemetry.record_delete_attempt(Store::File, &result);
    result
}
