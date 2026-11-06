use super::*;
use crate::account_lease::AuthRefreshGuard;

impl ObservedStorage {
    pub(super) fn compare_and_save_observed(
        &self,
        expected: &AuthDotJson,
        auth: &AuthDotJson,
        guard: Option<&AuthRefreshGuard>,
    ) -> std::io::Result<()> {
        let mut telemetry = telemetry(self.mode, self.kind, Operation::Save);
        let result = match guard {
            Some(guard) => self
                .inner
                .compare_and_save_with_guard(expected, auth, guard),
            None => self.inner.compare_and_save(expected, auth),
        };
        telemetry.record_save_attempt(self.store(), &result);
        if self.mode == AuthCredentialsStoreMode::Keyring
            && let Err(error) = &result
        {
            telemetry.record_secure_error(error);
        }
        result
    }
    pub(super) fn load_guarded(
        &self,
        guard: &AuthRefreshGuard,
    ) -> std::io::Result<Option<AuthDotJson>> {
        let mut telemetry = telemetry(self.mode, self.kind, Operation::Load);
        let result = self.inner.load_with_guard(guard);
        telemetry.record_load_attempt(self.store(), &result);
        if self.mode == AuthCredentialsStoreMode::Keyring
            && let Err(error) = &result
        {
            telemetry.record_secure_error(error);
        }
        result
    }

    pub(super) fn save_guarded(
        &self,
        auth: &AuthDotJson,
        guard: &AuthRefreshGuard,
    ) -> std::io::Result<()> {
        let mut telemetry = telemetry(self.mode, self.kind, Operation::Save);
        let result = self.inner.save_with_guard(auth, guard);
        telemetry.record_save_attempt(self.store(), &result);
        if self.mode == AuthCredentialsStoreMode::Keyring
            && let Err(error) = &result
        {
            telemetry.record_secure_error(error);
        }
        result
    }

    pub(super) fn save_preserving_fallback(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        let mut telemetry = telemetry(self.mode, self.kind, Operation::Save);
        let result = self.inner.save_preserving_file(auth);
        telemetry.record_save_attempt(self.store(), &result);
        if self.mode == AuthCredentialsStoreMode::Keyring
            && let Err(error) = &result
        {
            telemetry.record_secure_error(error);
        }
        result
    }

    pub(super) fn delete_guarded(&self, guard: &AuthRefreshGuard) -> std::io::Result<bool> {
        let store = match self.mode {
            AuthCredentialsStoreMode::File | AuthCredentialsStoreMode::Ephemeral => self.store(),
            AuthCredentialsStoreMode::Auto | AuthCredentialsStoreMode::Keyring => Store::Multiple,
        };
        let mut telemetry = telemetry(self.mode, self.kind, Operation::Delete);
        let result = self.inner.delete_with_guard(guard);
        telemetry.record_delete_attempt(store, &result);
        result
    }
}

#[cfg(test)]
#[path = "guarded_tests.rs"]
mod tests;
