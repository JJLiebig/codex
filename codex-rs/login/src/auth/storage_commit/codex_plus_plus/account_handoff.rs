//! Conditional root handoff under the upstream credential commit lock.
use super::*;

pub(in crate::auth::storage) fn save_file_if_unchanged(
    home: &Path,
    expected: &AuthDotJson,
    auth: &AuthDotJson,
    guard: &AuthRefreshGuard,
) -> std::io::Result<()> {
    let file = Arc::new(FileAuthStorage::new(home.to_path_buf()));
    let storage = CommittedStorage {
        inner: file.clone(),
        home: home.to_path_buf(),
        mode: AuthCredentialsStoreMode::File,
    };
    let _commit = storage.lock()?;
    if file.load_with_guard(guard)?.as_ref() != Some(expected) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "Account credentials changed before root handoff.",
        ));
    }
    file.save_with_guard(auth, guard)
}
