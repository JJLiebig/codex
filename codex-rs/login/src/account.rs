use chrono::Utc;
use codex_config::types::AuthCredentialsStoreMode;
use codex_protocol::auth::AuthMode;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::fmt;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use crate::AuthDotJson;
use crate::AuthKeyringBackendKind;
use crate::account_lease::AccountLease;
use crate::account_lease::AuthRefreshGuard;
use crate::auth::load_auth_dot_json_with_guard;
use crate::auth::save_auth_with_guard;
use crate::auth::save_file_auth_if_unchanged;
use crate::load_auth_dot_json;
use crate::token_data::TokenData;

#[path = "codex_plus_plus/account_bridge.rs"]
mod account_bridge;
#[path = "codex_plus_plus/account_policy.rs"]
pub(crate) mod account_policy;
#[path = "codex_plus_plus/account_removal.rs"]
mod account_removal;
#[path = "codex_plus_plus/reset_state.rs"]
pub(crate) mod reset_state;
#[path = "codex_plus_plus/weekly_window_state.rs"]
pub(crate) mod weekly_window_state;

pub use account_bridge::AccountHandoffOutcome;
const ACCOUNTS_DIR: &str = "accounts";
const INDEX_FILE: &str = "index.json";
const INDEX_LOCK_FILE: &str = "index.lock";

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct AccountId(String);

impl AccountId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AccountProfile {
    pub id: AccountId,
    pub label: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_enabled")]
    pub automation_enabled: bool,
    #[serde(default)]
    pub login_required: bool,
    #[serde(default)]
    pub priority: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limit_resets_at: Option<i64>,
    pub auth: AccountAuthStorage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountCandidate {
    pub id: AccountId,
    pub display_label: String,
    pub priority: u32,
    pub enabled: bool,
    pub automation_enabled: bool,
    pub usage_limit_resets_at: Option<i64>,
    pub blocked: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AccountAuthStorage {
    pub scope: AccountAuthScope,
    pub path: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountAuthScope {
    File,
}

#[derive(Clone, Debug)]
pub struct AccountStore {
    codex_home: PathBuf,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct AccountIndex {
    #[serde(default)]
    accounts: Vec<AccountProfile>,
}

impl AccountStore {
    pub fn new(codex_home: PathBuf) -> Self {
        Self { codex_home }
    }

    pub fn import_current(
        &self,
        label: Option<String>,
        root_store_mode: AuthCredentialsStoreMode,
        root_keyring_backend_kind: AuthKeyringBackendKind,
    ) -> std::io::Result<AccountProfile> {
        let _topology_lease = self.acquire_account_topology_lease()?;
        let source_account_id = {
            let root_refresh_guard = AuthRefreshGuard::acquire(&self.codex_home)?;
            let root_auth = load_auth_dot_json_with_guard(
                &self.codex_home,
                root_store_mode,
                root_keyring_backend_kind,
                &root_refresh_guard,
            )?
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "not logged in"))?;
            account_id_for_auth(&root_auth)?
        };
        let _reset_lease = self.acquire_reset_mutation_lease(&source_account_id)?;
        let root_refresh_guard = AuthRefreshGuard::acquire(&self.codex_home)?;
        let root_auth = load_auth_dot_json_with_guard(
            &self.codex_home,
            root_store_mode,
            root_keyring_backend_kind,
            &root_refresh_guard,
        )?
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "not logged in"))?;
        if account_id_for_auth(&root_auth)? != source_account_id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "current account changed while preparing import; retry",
            ));
        }
        let mut auth = root_auth.clone();
        let imported_from_root_marker = is_root_account_marker(&auth);
        let account_home = self.account_home(&source_account_id);
        let account_refresh_guard = AuthRefreshGuard::acquire(&account_home)?;
        if imported_from_root_marker {
            auth = load_auth_dot_json_with_guard(
                &account_home,
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default(),
                &account_refresh_guard,
            )?
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "current imported account is missing auth.json",
                )
            })?;
        }
        if !is_managed_chatgpt_auth(&auth) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "current auth is not a ChatGPT login",
            ));
        }
        if auth
            .tokens
            .as_ref()
            .is_none_or(|tokens| tokens.refresh_token.trim().is_empty())
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "current ChatGPT login is missing a refresh token",
            ));
        }

        let account_id = account_id_for_auth(&auth)?;
        if account_id != source_account_id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "root account marker does not match imported account auth",
            ));
        }
        let label = account_label_for_auth(&auth, label, &account_id)?;
        let _index_guard = self.acquire_index_lock()?;
        let previous_account_auth = load_auth_dot_json_with_guard(
            &account_home,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
            &account_refresh_guard,
        )?;
        let mut index = self.load_index()?;
        let previous_index = index.clone();
        save_auth_with_guard(
            &account_home,
            &auth,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
            &account_refresh_guard,
        )?;

        let auth_storage = AccountAuthStorage {
            scope: AccountAuthScope::File,
            path: auth_path_for_id(&account_id),
        };
        let existing = index
            .accounts
            .iter()
            .find(|profile| profile.id == account_id);
        let priority = existing
            .map(|profile| profile.priority)
            .unwrap_or_else(|| next_priority(&index.accounts));
        let usage_limit_resets_at = existing.and_then(|profile| profile.usage_limit_resets_at);
        let automation_enabled = existing.is_none_or(|profile| profile.automation_enabled);
        let profile = AccountProfile {
            id: account_id,
            label,
            enabled: true,
            automation_enabled,
            login_required: imported_from_root_marker
                && existing.is_some_and(|profile| profile.login_required),
            priority,
            usage_limit_resets_at,
            auth: auth_storage,
        };

        index.accounts.retain(|existing| existing.id != profile.id);
        index.accounts.push(profile.clone());
        sort_profiles(&mut index.accounts);
        if let Err(err) = self.save_index(&index) {
            return match restore_file_auth(
                &account_home,
                previous_account_auth.as_ref(),
                &account_refresh_guard,
            ) {
                Ok(()) => Err(err),
                Err(rollback_err) => Err(std::io::Error::other(format!(
                    "failed to update imported account index: {err}; failed to restore account auth: {rollback_err}"
                ))),
            };
        }
        if let Err(err) = save_root_account_marker(
            &self.codex_home,
            &auth,
            root_store_mode,
            root_keyring_backend_kind,
            &root_refresh_guard,
        ) {
            let mut rollback_errors = Vec::new();
            if let Err(rollback_err) = self.save_index(&previous_index) {
                rollback_errors.push(format!("restore account index: {rollback_err}"));
            }
            if let Err(rollback_err) = restore_file_auth(
                &account_home,
                previous_account_auth.as_ref(),
                &account_refresh_guard,
            ) {
                rollback_errors.push(format!("restore account auth: {rollback_err}"));
            }
            if let Err(rollback_err) = save_auth_with_guard(
                &self.codex_home,
                &root_auth,
                root_store_mode,
                root_keyring_backend_kind,
                &root_refresh_guard,
            ) {
                rollback_errors.push(format!("restore root auth: {rollback_err}"));
            }
            return if rollback_errors.is_empty() {
                Err(err)
            } else {
                Err(std::io::Error::other(format!(
                    "failed to save imported account marker: {err}; rollback failed: {}",
                    rollback_errors.join("; ")
                )))
            };
        }
        Ok(profile)
    }

    pub fn list(&self) -> std::io::Result<Vec<AccountProfile>> {
        let mut accounts = self.load_index()?.accounts;
        sort_profiles(&mut accounts);
        Ok(accounts)
    }

    pub fn candidates(&self) -> std::io::Result<Vec<AccountCandidate>> {
        self.candidates_at(Utc::now().timestamp())
    }

    pub(crate) fn candidates_at(&self, now: i64) -> std::io::Result<Vec<AccountCandidate>> {
        Ok(self
            .list()?
            .into_iter()
            .map(|profile| AccountCandidate {
                blocked: profile
                    .usage_limit_resets_at
                    .is_some_and(|resets_at| resets_at > now),
                id: profile.id,
                display_label: profile.label,
                priority: profile.priority,
                enabled: profile.enabled,
                automation_enabled: profile.automation_enabled,
                usage_limit_resets_at: profile.usage_limit_resets_at,
            })
            .collect())
    }

    pub fn record_usage_limit_resets_at(
        &self,
        account_id: &AccountId,
        resets_at: i64,
    ) -> std::io::Result<bool> {
        let _index_guard = self.acquire_index_lock()?;
        let mut index = self.load_index()?;
        let Some(account) = index
            .accounts
            .iter_mut()
            .find(|account| &account.id == account_id)
        else {
            return Ok(false);
        };

        account.usage_limit_resets_at = Some(resets_at);
        sort_profiles(&mut index.accounts);
        self.save_index(&index)?;
        Ok(true)
    }

    pub fn record_login_required(&self, account_id: &AccountId) -> std::io::Result<bool> {
        let _index_guard = self.acquire_index_lock()?;
        self.record_login_required_unlocked(account_id)
    }

    pub fn record_login_required_if_auth_matches(
        &self,
        account_id: &AccountId,
        expected_auth: &AuthDotJson,
    ) -> std::io::Result<bool> {
        let account_home = self.account_home(account_id);
        let refresh_guard = AuthRefreshGuard::acquire(&account_home)?;
        let current_auth = load_auth_dot_json_with_guard(
            &account_home,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
            &refresh_guard,
        )?;
        if current_auth.as_ref() != Some(expected_auth) {
            return Ok(false);
        }
        let _index_guard = self.acquire_index_lock()?;
        self.record_login_required_unlocked(account_id)
    }

    fn record_login_required_unlocked(&self, account_id: &AccountId) -> std::io::Result<bool> {
        let mut index = self.load_index()?;
        let Some(account) = index
            .accounts
            .iter_mut()
            .find(|account| &account.id == account_id)
        else {
            return Ok(false);
        };
        if account.login_required {
            return Ok(true);
        }

        account.login_required = true;
        self.save_index(&index)?;
        Ok(true)
    }

    pub fn apply_imported_account_to_root_auth(
        &self,
        account_id: &AccountId,
        root_store_mode: AuthCredentialsStoreMode,
        root_keyring_backend_kind: AuthKeyringBackendKind,
    ) -> std::io::Result<AccountProfile> {
        let account_home = self.account_home(account_id);
        let root_refresh_guard = AuthRefreshGuard::acquire(&self.codex_home)?;
        let account_refresh_guard = AuthRefreshGuard::acquire(&account_home)?;
        let _index_guard = self.acquire_index_lock()?;
        let (profile, account_home) = self
            .file_account_profiles()?
            .into_iter()
            .find(|(profile, _)| profile.enabled && &profile.id == account_id)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("imported account {account_id} is not enabled or does not exist"),
                )
            })?;
        let auth = load_auth_dot_json_with_guard(
            &account_home,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
            &account_refresh_guard,
        )?
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("imported account {account_id} is missing auth.json"),
            )
        })?;

        save_root_account_marker(
            &self.codex_home,
            &auth,
            root_store_mode,
            root_keyring_backend_kind,
            &root_refresh_guard,
        )?;
        Ok(profile)
    }

    pub(crate) fn disable_all_unlocked(&self) -> std::io::Result<bool> {
        let mut index = self.load_index()?;
        let mut changed = false;
        for account in &mut index.accounts {
            if account.enabled {
                account.enabled = false;
                changed = true;
            }
        }
        if changed {
            self.save_index(&index)?;
        }
        Ok(changed)
    }

    pub fn enabled_file_accounts(&self) -> std::io::Result<Vec<(AccountId, PathBuf)>> {
        Ok(self
            .enabled_file_account_profiles()?
            .into_iter()
            .map(|(account, home)| (account.id, home))
            .collect())
    }

    pub fn current_root_account_id(
        &self,
        root_store_mode: AuthCredentialsStoreMode,
        root_keyring_backend_kind: AuthKeyringBackendKind,
    ) -> std::io::Result<Option<AccountId>> {
        load_auth_dot_json(&self.codex_home, root_store_mode, root_keyring_backend_kind)?
            .map(|auth| account_id_for_auth(&auth))
            .transpose()
    }

    pub fn account_in_use(&self, account_id: &AccountId) -> std::io::Result<bool> {
        Ok(self.try_acquire_lease(account_id)?.is_none())
    }

    pub(crate) fn try_acquire_lease(
        &self,
        account_id: &AccountId,
    ) -> std::io::Result<Option<AccountLease>> {
        AccountLease::try_acquire(
            &self
                .accounts_dir()
                .join("leases")
                .join(format!("{account_id}.lock")),
        )
    }

    pub(crate) fn enabled_file_account_profiles(
        &self,
    ) -> std::io::Result<Vec<(AccountProfile, PathBuf)>> {
        Ok(self
            .file_account_profiles()?
            .into_iter()
            .filter(|(account, _)| account.enabled && !account.login_required)
            .collect())
    }

    pub(crate) fn file_account_profiles(&self) -> std::io::Result<Vec<(AccountProfile, PathBuf)>> {
        Ok(self
            .list()?
            .into_iter()
            .filter_map(|account| match account.auth.scope {
                AccountAuthScope::File => {
                    let home = self.account_home(&account.id);
                    home.join("auth.json").is_file().then_some((account, home))
                }
            })
            .collect())
    }

    pub(crate) fn file_auth_homes(&self) -> std::io::Result<Vec<PathBuf>> {
        let entries = match std::fs::read_dir(self.accounts_dir()) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let mut auth_homes = Vec::new();
        for entry in entries {
            let account_home = entry?.path();
            if account_home.join("auth.json").is_file() {
                auth_homes.push(account_home);
            }
        }
        auth_homes.sort();
        Ok(auth_homes)
    }

    pub(crate) fn account_home(&self, account_id: &AccountId) -> PathBuf {
        self.accounts_dir().join(account_id.as_str())
    }

    fn accounts_dir(&self) -> PathBuf {
        self.codex_home.join(ACCOUNTS_DIR)
    }

    fn index_path(&self) -> PathBuf {
        self.accounts_dir().join(INDEX_FILE)
    }

    pub(crate) fn acquire_index_lock(&self) -> std::io::Result<AccountLease> {
        AccountLease::acquire(&self.accounts_dir().join(INDEX_LOCK_FILE))
    }

    fn load_index(&self) -> std::io::Result<AccountIndex> {
        let path = self.index_path();
        let data = match std::fs::read_to_string(&path) {
            Ok(data) => data,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AccountIndex::default());
            }
            Err(err) => return Err(err),
        };
        serde_json::from_str(&data).map_err(std::io::Error::other)
    }

    fn save_index(&self, index: &AccountIndex) -> std::io::Result<()> {
        let accounts_dir = self.accounts_dir();
        std::fs::create_dir_all(&accounts_dir)?;
        let temp_path = accounts_dir.join("index.json.tmp");
        let json = serde_json::to_vec_pretty(index).map_err(std::io::Error::other)?;
        {
            let mut file = std::fs::File::create(&temp_path)?;
            file.write_all(&json)?;
            file.flush()?;
        }
        replace_file(&temp_path, &self.index_path())
    }
}

#[cfg(not(windows))]
pub(crate) fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(windows)]
pub(crate) fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    use std::iter;
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    let from: Vec<u16> = from
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(iter::once(0)).collect();

    let replaced = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn account_label_for_auth(
    auth: &AuthDotJson,
    label: Option<String>,
    account_id: &AccountId,
) -> std::io::Result<String> {
    if let Some(label) = label {
        let label = label.trim().to_string();
        if label.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "account label cannot be empty",
            ));
        }
        return Ok(label);
    }

    Ok(auth
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.id_token.email.as_deref())
        .map(str::trim)
        .filter(|email| !email.is_empty())
        .unwrap_or_else(|| account_id.as_str())
        .to_string())
}

pub(crate) fn account_id_for_auth(auth: &AuthDotJson) -> std::io::Result<AccountId> {
    let tokens = auth.tokens.as_ref().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "current auth is not a ChatGPT login",
        )
    })?;

    account_id_for_token_data(tokens)
}

fn account_id_for_token_data(tokens: &TokenData) -> std::io::Result<AccountId> {
    let identity = tokens
        .account_id
        .as_deref()
        .or(tokens.id_token.chatgpt_account_id.as_deref())
        .map(|value| ("account", value.to_string()))
        .or_else(|| {
            tokens
                .id_token
                .chatgpt_user_id
                .as_deref()
                .map(|value| ("user", value.to_string()))
        })
        .or_else(|| {
            tokens
                .id_token
                .email
                .as_deref()
                .map(|value| ("email", value.to_ascii_lowercase()))
        })
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "current ChatGPT auth is missing a stable account identity",
            )
        })?;

    Ok(account_id_for_stable_identity(identity.0, &identity.1))
}

pub(super) fn account_id_for_workspace(account_id: &str) -> AccountId {
    account_id_for_stable_identity("account", account_id)
}

fn account_id_for_stable_identity(kind: &str, value: &str) -> AccountId {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update(b":");
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    AccountId(format!("acct_{}", &hex[..16]))
}

fn is_managed_chatgpt_auth(auth: &AuthDotJson) -> bool {
    match auth.auth_mode {
        Some(AuthMode::Chatgpt) => true,
        Some(
            AuthMode::ApiKey
            | AuthMode::Headers
            | AuthMode::ChatgptAuthTokens
            | AuthMode::AgentIdentity
            | AuthMode::PersonalAccessToken
            | AuthMode::BedrockApiKey
            | AuthMode::BedrockAccessKeys,
        ) => false,
        None => auth.openai_api_key.is_none() && auth.tokens.is_some(),
    }
}

pub(crate) fn is_root_account_marker(auth: &AuthDotJson) -> bool {
    is_managed_chatgpt_auth(auth)
        && auth
            .tokens
            .as_ref()
            .is_some_and(|tokens| tokens.refresh_token.is_empty())
}

fn save_root_account_marker(
    codex_home: &Path,
    auth: &AuthDotJson,
    store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
    guard: &AuthRefreshGuard,
) -> std::io::Result<()> {
    let mut marker = auth.clone();
    if let Some(tokens) = marker.tokens.as_mut() {
        tokens.refresh_token.clear();
    }
    save_auth_with_guard(codex_home, &marker, store_mode, keyring_backend_kind, guard)
}

fn restore_file_auth(
    auth_home: &Path,
    auth: Option<&AuthDotJson>,
    guard: &AuthRefreshGuard,
) -> std::io::Result<()> {
    if let Some(auth) = auth {
        return save_auth_with_guard(
            auth_home,
            auth,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
            guard,
        );
    }
    match std::fs::remove_file(auth_home.join("auth.json")) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn auth_path_for_id(account_id: &AccountId) -> String {
    format!("{ACCOUNTS_DIR}/{account_id}/auth.json")
}

fn next_priority(accounts: &[AccountProfile]) -> u32 {
    accounts
        .iter()
        .map(|profile| profile.priority)
        .max()
        .map_or(0, |priority| priority.saturating_add(1))
}

fn sort_profiles(accounts: &mut [AccountProfile]) {
    accounts.sort_by(|a, b| (a.priority, &a.id).cmp(&(b.priority, &b.id)));
}

fn default_enabled() -> bool {
    true
}

#[cfg(test)]
#[path = "account_tests.rs"]
mod tests;
