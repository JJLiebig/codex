//! One native authority refresh for a proved, captured owned upstream 401.
use super::super::*;
use super::imported_account_refresh::native_source_changed;
use super::native_credential_export::NativeRequestAdmission;

impl UnauthorizedRecovery {
    /// Consume one authority attempt for the exact native credential used by an owned request.
    pub async fn next_for_native_request(
        &mut self,
        expected: &NativeCredentialExpectation,
    ) -> Result<UnauthorizedRecoveryStepResult, RefreshTokenError> {
        if !self.has_next() {
            return Err(native_source_changed());
        }
        // Replaying an unchanged publication after reload alone hits the proxy's suspension.
        // Consume the existing budget once, reloading and refreshing under the same guard.
        self.step = UnauthorizedRecoveryStep::Done;
        let manager = Arc::clone(&self.manager);
        let _refresh_guard = manager
            .refresh_lock
            .acquire()
            .await
            .map_err(|_| native_source_changed())?;
        let guard = manager
            .native_request_guard(expected, NativeRequestAdmission::Eligible)
            .await
            .map_err(RefreshTokenError::Transient)?
            .ok_or_else(native_source_changed)?;
        let account_id = manager.auth_cached().and_then(|auth| auth.get_account_id());
        if matches!(
            manager
                .reload_if_account_id_matches(account_id.as_deref(), Some(&guard))
                .await,
            ReloadOutcome::Skipped
        ) {
            return Err(native_source_changed());
        }
        let mut expected = expected.clone();
        expected.revision = *manager.auth_change_receiver().borrow();
        if !manager
            .native_request_matches(&expected, NativeRequestAdmission::Eligible, &guard)
            .map_err(RefreshTokenError::Transient)?
        {
            return Err(native_source_changed());
        }
        let result = manager
            .refresh_token_from_authority_impl(Some(&guard))
            .await;
        manager
            .recover_terminal_imported_refresh(
                result,
                manager.active_account_id(),
                guard,
                Some(&expected),
            )
            .await?;
        Ok(UnauthorizedRecoveryStepResult {
            auth_state_changed: Some(true),
        })
    }
}
