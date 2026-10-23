use super::super::*;

pub(in crate::auth::manager) fn publish(
    manager: &AuthManager,
    generation: u64,
    loaded_auth: LoadedAuth,
) -> Option<AuthReload> {
    let mut guard = manager.inner.write().ok()?;
    if manager.auth_change_state_tx.borrow().generation != generation {
        return None;
    }
    let active_account_id_before_reload = manager.active_account_id();
    if let (Some(account_id), Some(auth)) = (
        active_account_id_before_reload.as_ref(),
        loaded_auth.auth.as_ref(),
    ) && auth
        .get_current_auth_json()
        .and_then(|auth| account_id_for_auth(&auth).ok())
        .as_ref()
        != Some(account_id)
    {
        manager.clear_active_imported_account();
    }
    if let Some((account_id, account_home)) = loaded_auth.imported_source {
        manager.set_active_imported_account_source(account_id, account_home);
    }
    let active_account_changed = active_account_id_before_reload != manager.active_account_id();
    let changed_for_refresh = active_account_changed
        || !AuthManager::auths_equal_for_refresh(guard.auth.as_ref(), loaded_auth.auth.as_ref());
    // An unchanged reload must retain the selected storage backend and its authority.
    let changed = changed_for_refresh
        && (manager.set_cached_auth_locked(&mut guard, loaded_auth.auth) || active_account_changed);
    Some(AuthReload {
        changed,
        changed_for_refresh,
    })
}
