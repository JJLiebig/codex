//! Compose account coordination with the upstream credential commit lock.
use super::*;

impl CommittedStorage {
    pub(super) fn save_guarded(
        &self,
        auth: &AuthDotJson,
        guard: &AuthRefreshGuard,
    ) -> std::io::Result<()> {
        guard.ensure_matches(&self.home)?;
        let _commit = self.lock()?;
        self.inner.save_with_guard(auth, guard)
    }
    pub(super) fn compare_and_save_guarded(
        &self,
        expected: &AuthDotJson,
        auth: &AuthDotJson,
        guard: &AuthRefreshGuard,
    ) -> std::io::Result<()> {
        guard.ensure_matches(&self.home)?;
        let _commit = self.lock()?;
        self.inner
            .compare_and_save_with_guard(expected, auth, guard)
    }
    pub(super) fn delete_guarded(&self, guard: &AuthRefreshGuard) -> std::io::Result<bool> {
        guard.ensure_matches(&self.home)?;
        let _commit = self.lock()?;
        self.inner.delete_with_guard(guard)
    }
}

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
