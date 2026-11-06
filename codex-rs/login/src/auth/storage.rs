use chrono::DateTime;
use chrono::Utc;
#[cfg(test)]
use codex_otel::auth_storage::Operation;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::collections::HashMap;
use std::fmt::Debug;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tracing::warn;

#[path = "storage_error.rs"]
mod storage_error;

#[path = "storage_file.rs"]
mod storage_file;

#[path = "storage_commit.rs"]
mod storage_commit;

#[path = "storage_telemetry.rs"]
mod storage_telemetry;

use super::BedrockAccessKeysAuth;
use super::BedrockApiKeyAuth;
use crate::token_data::TokenData;
use codex_agent_identity::AgentIdentityJwtClaims;
use codex_agent_identity::decode_agent_identity_jwt;
use codex_config::types::AuthCredentialsStoreMode;
pub use codex_config::types::AuthKeyringBackendKind;
use codex_keyring_store::DefaultKeyringStore;
use codex_keyring_store::KeyringStore;
use codex_protocol::account::PlanType as AccountPlanType;
use codex_protocol::auth::AuthMode;
use codex_secrets::LocalSecretsNamespace;
use codex_secrets::SecretName;
use codex_secrets::SecretScope;
use codex_secrets::SecretsBackendKind;
use codex_secrets::SecretsManager;
use once_cell::sync::Lazy;

mod codex_plus_plus;
pub(crate) use codex_plus_plus::account_removal;

use crate::account_lease::AuthRefreshGuard;
use codex_plus_plus::FileAuthorityMarker;

/// Expected structure for $CODEX_HOME/auth.json.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct AuthDotJson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_mode: Option<AuthMode>,

    #[serde(rename = "OPENAI_API_KEY")]
    pub openai_api_key: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenData>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<DateTime<Utc>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_identity: Option<AgentIdentityStorage>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personal_access_token: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bedrock_api_key: Option<BedrockApiKeyAuth>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bedrock_access_keys: Option<BedrockAccessKeysAuth>,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(untagged)]
pub enum AgentIdentityStorage {
    Jwt(String),
    Record(AgentIdentityAuthRecord),
}

impl AgentIdentityStorage {
    pub fn has_auth_material(&self) -> bool {
        match self {
            Self::Jwt(jwt) => !jwt.trim().is_empty(),
            Self::Record(record) => {
                !record.agent_runtime_id.trim().is_empty()
                    && !record.agent_private_key.trim().is_empty()
            }
        }
    }

    pub(crate) fn as_record(&self) -> Option<&AgentIdentityAuthRecord> {
        match self {
            Self::Jwt(_) => None,
            Self::Record(record) => Some(record),
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct AgentIdentityAuthRecord {
    pub agent_runtime_id: String,
    pub agent_private_key: String,
    pub account_id: String,
    pub chatgpt_user_id: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_empty_string",
        serialize_with = "serialize_optional_string_as_empty"
    )]
    pub email: Option<String>,
    pub plan_type: AccountPlanType,
    pub chatgpt_account_is_fedramp: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

fn deserialize_optional_non_empty_string<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(|value| value.filter(|value| !value.is_empty()))
}

fn serialize_optional_string_as_empty<S>(
    value: &Option<String>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    value.as_deref().unwrap_or_default().serialize(serializer)
}

impl AgentIdentityAuthRecord {
    pub(crate) fn from_agent_identity_jwt(jwt: &str) -> std::io::Result<Self> {
        let claims = decode_agent_identity_jwt(jwt, /*jwks*/ None).map_err(|error| {
            storage_error::with_context("failed to decode agent identity JWT", error)
        })?;

        Ok(claims.into())
    }
}

impl From<AgentIdentityJwtClaims> for AgentIdentityAuthRecord {
    fn from(claims: AgentIdentityJwtClaims) -> Self {
        Self {
            agent_runtime_id: claims.agent_runtime_id,
            agent_private_key: claims.agent_private_key,
            account_id: claims.account_id,
            chatgpt_user_id: claims.chatgpt_user_id,
            email: claims.email,
            plan_type: claims.plan_type.into(),
            chatgpt_account_is_fedramp: claims.chatgpt_account_is_fedramp,
            task_id: None,
        }
    }
}

pub(super) fn get_auth_file(codex_home: &Path) -> PathBuf {
    codex_home.join("auth.json")
}

pub(super) fn delete_file_if_exists(codex_home: &Path) -> std::io::Result<bool> {
    let auth_file = get_auth_file(codex_home);
    match std::fs::remove_file(&auth_file) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

// Protect in-process read-modify-write commits, never network requests.
static AUTH_STORAGE_COMMIT: Mutex<()> = Mutex::new(());

pub(super) fn auth_storage_commit_guard() -> std::io::Result<std::sync::MutexGuard<'static, ()>> {
    AUTH_STORAGE_COMMIT
        .lock()
        .map_err(|_| std::io::Error::other("Auth storage commit lock is unavailable."))
}

pub(super) trait AuthStorageBackend: Debug + Send + Sync {
    fn persistent_home(&self) -> Option<&Path> {
        None
    }

    /// Serialize the whole read/modify/write across managers and processes.
    fn lock_for_write(
        &self,
    ) -> std::io::Result<(std::sync::MutexGuard<'static, ()>, Option<File>)> {
        let guard = auth_storage_commit_guard()?;
        let file = if let Some(home) = self.persistent_home() {
            std::fs::create_dir_all(home)?;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(home.join("auth.lock"))?;
            file.lock()?;
            Some(file)
        } else {
            None
        };
        Ok((guard, file))
    }

    fn load(&self) -> std::io::Result<Option<AuthDotJson>>;
    fn load_with_guard(&self, _guard: &AuthRefreshGuard) -> std::io::Result<Option<AuthDotJson>> {
        self.load()
    }
    /// An unavailable keyring cannot authorize using an older fallback record.
    fn load_account_grant(&self) -> std::io::Result<Option<AuthDotJson>> {
        self.load()
    }
    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()>;
    fn save_with_guard(
        &self,
        auth: &AuthDotJson,
        _guard: &AuthRefreshGuard,
    ) -> std::io::Result<()> {
        self.save(auth)
    }
    fn save_preserving_file(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        self.save(auth)
    }
    /// Commits only while the selected credential document remains unchanged.
    fn compare_and_save(&self, expected: &AuthDotJson, auth: &AuthDotJson) -> std::io::Result<()> {
        // An unavailable keyring cannot prove that a fallback is still current.
        // Do not overwrite a recovered secure-store generation we could not read.
        if self.load_account_grant()?.as_ref() != Some(expected) {
            return Err(std::io::Error::other(
                "Credentials changed before refresh commit.",
            ));
        }

        self.save(auth)
    }
    fn compare_and_save_with_guard(
        &self,
        expected: &AuthDotJson,
        auth: &AuthDotJson,
        guard: &AuthRefreshGuard,
    ) -> std::io::Result<()> {
        if self.load_with_guard(guard)?.as_ref() != Some(expected) {
            return Err(std::io::Error::other(
                "Credentials changed before refresh commit.",
            ));
        }
        self.save_with_guard(auth, guard)
    }
    fn delete(&self) -> std::io::Result<bool>;
    fn delete_with_guard(&self, _guard: &AuthRefreshGuard) -> std::io::Result<bool> {
        self.delete()
    }
}

#[derive(Debug)]
pub(super) struct FileAuthStorage {
    codex_home: PathBuf,
}

impl FileAuthStorage {
    pub(super) fn new(codex_home: PathBuf) -> Self {
        Self { codex_home }
    }

    /// Attempt to read and parse the `auth.json` file in the given `CODEX_HOME` directory.
    /// Returns the full AuthDotJson structure.
    pub(super) fn try_read_auth_json(&self, auth_file: &Path) -> std::io::Result<AuthDotJson> {
        let mut file = File::open(auth_file)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        let auth_dot_json: AuthDotJson = serde_json::from_str(&contents).map_err(|err| {
            storage_error::with_context("failed to deserialize CLI auth from file", err)
        })?;

        Ok(auth_dot_json)
    }
}

impl AuthStorageBackend for FileAuthStorage {
    fn persistent_home(&self) -> Option<&Path> {
        Some(&self.codex_home)
    }

    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        let auth_file = get_auth_file(&self.codex_home);
        let auth_dot_json = match self.try_read_auth_json(&auth_file) {
            Ok(auth) => auth,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        Ok(Some(auth_dot_json))
    }

    fn save(&self, auth_dot_json: &AuthDotJson) -> std::io::Result<()> {
        let guard = AuthRefreshGuard::acquire(&self.codex_home)?;
        self.save_with_guard(auth_dot_json, &guard)
    }

    fn load_with_guard(&self, guard: &AuthRefreshGuard) -> std::io::Result<Option<AuthDotJson>> {
        guard.ensure_matches(&self.codex_home)?;
        self.load()
    }

    fn save_with_guard(
        &self,
        auth_dot_json: &AuthDotJson,
        guard: &AuthRefreshGuard,
    ) -> std::io::Result<()> {
        guard.ensure_matches(&self.codex_home)?;
        let auth_file = storage_file::resolve_auth_file(get_auth_file(&self.codex_home))?;
        let parent = auth_file
            .parent()
            .ok_or_else(|| std::io::Error::other("auth file has no parent"))?;
        std::fs::create_dir_all(parent)?;
        let json_data = serde_json::to_string_pretty(auth_dot_json)?;
        storage_file::check_replaceable(&auth_file)?;
        // tempfile creates a private (0600 on Unix) file in the same filesystem.
        // Readers see the complete old or new document, never a truncated auth.json.
        let mut replacement = storage_file::replacement(&auth_file)?;
        replacement.write_all(json_data.as_bytes())?;
        replacement.as_file().sync_all()?;
        // Clear Windows' temporary attribute, then restore cleanup protection.
        // std::fs::rename can replace a file held open by a reader on Windows;
        // tempfile::persist only uses MoveFileExW, which rejects that case.
        let (_file, path) = replacement.keep().map_err(|error| error.error)?;
        let mut path = tempfile::TempPath::try_from_path(path)?;
        std::fs::rename(&path, auth_file)?;
        path.disable_cleanup(/*disable_cleanup*/ true);
        Ok(())
    }

    fn delete(&self) -> std::io::Result<bool> {
        let file_removed = delete_file_if_exists(&self.codex_home)?;
        let marker_removed = FileAuthorityMarker::new(&self.codex_home).clear()?;
        Ok(marker_removed || file_removed)
    }
}

static CODEX_AUTH_SECRET_NAME: Lazy<SecretName> =
    Lazy::new(|| match SecretName::new("CODEX_AUTH") {
        Ok(name) => name,
        Err(err) => unreachable!("CODEX_AUTH should be a valid secret name: {err}"),
    });
const KEYRING_SERVICE: &str = "Codex Auth";

// turns codex_home path into a stable, short key string
fn compute_store_key(codex_home: &Path) -> std::io::Result<String> {
    let canonical = codex_home
        .canonicalize()
        .unwrap_or_else(|_| codex_home.to_path_buf());
    let path_str = canonical.to_string_lossy();
    let mut hasher = Sha256::new();
    hasher.update(path_str.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    let truncated = hex.get(..16).unwrap_or(&hex);
    Ok(format!("cli|{truncated}"))
}

#[derive(Clone, Debug)]
struct DirectKeyringAuthStorage {
    codex_home: PathBuf,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
}

impl DirectKeyringAuthStorage {
    fn new(
        codex_home: PathBuf,
        keyring_store: Arc<dyn KeyringStore>,
        mode: AuthCredentialsStoreMode,
    ) -> Self {
        Self {
            codex_home,
            keyring_store,
            mode,
        }
    }

    fn load_from_keyring(&self, key: &str) -> std::io::Result<Option<AuthDotJson>> {
        match self
            .keyring_store
            .load(KEYRING_SERVICE, key)
            .map_err(std::io::Error::from)
            .map_err(|error| {
                storage_error::with_context("failed to load CLI auth from keyring", error)
            })? {
            Some(serialized) => serde_json::from_str(&serialized).map(Some).map_err(|err| {
                storage_error::with_context("failed to deserialize CLI auth from keyring", err)
            }),
            None => Ok(None),
        }
    }

    fn save_to_keyring(&self, key: &str, value: &str) -> std::io::Result<()> {
        self.keyring_store
            .save(KEYRING_SERVICE, key, value)
            .map_err(std::io::Error::from)
            .map_err(|error| {
                let error =
                    storage_error::with_context("failed to write OAuth tokens to keyring", error);
                if self.mode == AuthCredentialsStoreMode::Keyring {
                    warn!("{error}");
                }
                error
            })
    }

    fn save_guarded(&self, auth: &AuthDotJson, guard: &AuthRefreshGuard) -> std::io::Result<()> {
        let marker = FileAuthorityMarker::new(&self.codex_home);
        marker.prepare_keyring_save(auth, guard)?;
        self.save_preserving_file(auth)?;
        codex_plus_plus::cleanup_fallback(
            &self.codex_home,
            self.mode,
            AuthKeyringBackendKind::Direct,
        )?;
        marker.clear()?;
        Ok(())
    }

    fn delete_guarded(&self) -> std::io::Result<bool> {
        let file_removed = delete_file_if_exists(&self.codex_home);
        let keyring_removed = self.delete_keyring();
        let file_removed = file_removed?;
        let keyring_removed = keyring_removed?;
        let marker_removed = FileAuthorityMarker::new(&self.codex_home).clear()?;
        Ok(keyring_removed || marker_removed || file_removed)
    }

    fn delete_keyring(&self) -> std::io::Result<bool> {
        let key = compute_store_key(&self.codex_home)?;
        self.keyring_store
            .delete(KEYRING_SERVICE, &key)
            .map_err(std::io::Error::from)
            .map_err(|error| {
                storage_error::with_context("failed to delete auth from keyring", error)
            })
    }
}

impl AuthStorageBackend for DirectKeyringAuthStorage {
    fn persistent_home(&self) -> Option<&Path> {
        Some(&self.codex_home)
    }

    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        let key = compute_store_key(&self.codex_home)?;
        self.load_from_keyring(&key)
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        let guard = AuthRefreshGuard::acquire(&self.codex_home)?;
        self.save_with_guard(auth, &guard)
    }

    fn save_with_guard(&self, auth: &AuthDotJson, guard: &AuthRefreshGuard) -> std::io::Result<()> {
        guard.ensure_matches(&self.codex_home)?;
        self.save_guarded(auth, guard)
    }

    fn save_preserving_file(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        let key = compute_store_key(&self.codex_home)?;
        // Simpler error mapping per style: prefer method reference over closure
        let serialized = serde_json::to_string(auth).map_err(std::io::Error::other)?;
        self.save_to_keyring(&key, &serialized)
    }

    fn delete(&self) -> std::io::Result<bool> {
        let guard = AuthRefreshGuard::acquire(&self.codex_home)?;
        self.delete_with_guard(&guard)
    }

    fn delete_with_guard(&self, guard: &AuthRefreshGuard) -> std::io::Result<bool> {
        guard.ensure_matches(&self.codex_home)?;
        self.delete_guarded()
    }
}

#[derive(Clone)]
struct SecretsKeyringAuthStorage {
    codex_home: PathBuf,
    mode: AuthCredentialsStoreMode,
    direct_storage: DirectKeyringAuthStorage,
    secrets_manager: SecretsManager,
}

impl Debug for SecretsKeyringAuthStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretsKeyringAuthStorage")
            .field("codex_home", &self.codex_home)
            .finish_non_exhaustive()
    }
}

impl SecretsKeyringAuthStorage {
    fn new(
        codex_home: PathBuf,
        keyring_store: Arc<dyn KeyringStore>,
        mode: AuthCredentialsStoreMode,
    ) -> Self {
        let direct_storage =
            DirectKeyringAuthStorage::new(codex_home.clone(), Arc::clone(&keyring_store), mode);
        let secrets_manager = SecretsManager::new_with_keyring_store_and_namespace(
            codex_home.clone(),
            SecretsBackendKind::Local,
            keyring_store,
            LocalSecretsNamespace::CodexAuth,
        );
        Self {
            codex_home,
            mode,
            direct_storage,
            secrets_manager,
        }
    }

    fn save_to_keyring(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        let serialized = serde_json::to_string(auth).map_err(std::io::Error::other)?;
        self.secrets_manager
            .set(&SecretScope::Global, &CODEX_AUTH_SECRET_NAME, &serialized)
            .map_err(|error| {
                let error = storage_error::with_context(
                    "failed to write OAuth tokens to encrypted auth storage",
                    error,
                );
                if self.mode == AuthCredentialsStoreMode::Keyring {
                    warn!("{error}");
                }
                error
            })
    }

    fn save_guarded(&self, auth: &AuthDotJson, guard: &AuthRefreshGuard) -> std::io::Result<()> {
        let marker = FileAuthorityMarker::new(&self.codex_home);
        marker.prepare_keyring_save(auth, guard)?;
        self.save_to_keyring(auth)?;
        codex_plus_plus::cleanup_fallback(
            &self.codex_home,
            self.mode,
            AuthKeyringBackendKind::Secrets,
        )?;
        marker.clear()?;
        Ok(())
    }

    fn delete_guarded(&self) -> std::io::Result<bool> {
        let file_removed = delete_file_if_exists(&self.codex_home);
        let keyring_removed = self
            .secrets_manager
            .delete(&SecretScope::Global, &CODEX_AUTH_SECRET_NAME)
            .map_err(|error| {
                storage_error::with_context(
                    "failed to delete auth from encrypted auth storage",
                    error,
                )
            });
        let direct_removed = self.direct_storage.delete_keyring();
        let file_removed = file_removed?;
        let keyring_removed = keyring_removed?;
        let direct_removed = direct_removed?;
        let marker_removed = FileAuthorityMarker::new(&self.codex_home).clear()?;
        Ok(keyring_removed || direct_removed || marker_removed || file_removed)
    }
}

impl AuthStorageBackend for SecretsKeyringAuthStorage {
    fn persistent_home(&self) -> Option<&Path> {
        Some(&self.codex_home)
    }

    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        match self
            .secrets_manager
            .get(&SecretScope::Global, &CODEX_AUTH_SECRET_NAME)
            .map_err(|error| {
                storage_error::with_context(
                    "failed to load CLI auth from encrypted auth storage",
                    error,
                )
            })? {
            Some(serialized) => serde_json::from_str(&serialized).map(Some).map_err(|err| {
                storage_error::with_context(
                    "failed to deserialize CLI auth from encrypted auth storage",
                    err,
                )
            }),
            None => Ok(None),
        }
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        let guard = AuthRefreshGuard::acquire(&self.codex_home)?;
        self.save_with_guard(auth, &guard)
    }

    fn save_with_guard(&self, auth: &AuthDotJson, guard: &AuthRefreshGuard) -> std::io::Result<()> {
        guard.ensure_matches(&self.codex_home)?;
        self.save_guarded(auth, guard)
    }

    fn save_preserving_file(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        self.save_to_keyring(auth)
    }

    fn delete(&self) -> std::io::Result<bool> {
        let guard = AuthRefreshGuard::acquire(&self.codex_home)?;
        self.delete_with_guard(&guard)
    }

    fn delete_with_guard(&self, guard: &AuthRefreshGuard) -> std::io::Result<bool> {
        guard.ensure_matches(&self.codex_home)?;
        self.delete_guarded()
    }
}

#[derive(Clone, Debug)]
struct AutoAuthStorage {
    codex_home: PathBuf,
    keyring_storage: Arc<dyn AuthStorageBackend>,
    file_storage: Arc<FileAuthStorage>,
    file_authority: FileAuthorityMarker,
    keyring_backend_kind: AuthKeyringBackendKind,
}

impl AutoAuthStorage {
    fn new(
        codex_home: PathBuf,
        keyring_store: Arc<dyn KeyringStore>,
        keyring_backend_kind: AuthKeyringBackendKind,
    ) -> Self {
        let file_authority = FileAuthorityMarker::new(&codex_home);
        Self {
            keyring_storage: create_keyring_auth_storage(
                codex_home.clone(),
                keyring_store,
                keyring_backend_kind,
                AuthCredentialsStoreMode::Auto,
            ),
            file_storage: Arc::new(FileAuthStorage::new(codex_home.clone())),
            file_authority,
            codex_home,
            keyring_backend_kind,
        }
    }

    fn load_guarded(&self, guard: &AuthRefreshGuard) -> std::io::Result<Option<AuthDotJson>> {
        codex_plus_plus::load_auto_auth(self, guard)
    }

    fn save_guarded(&self, auth: &AuthDotJson, guard: &AuthRefreshGuard) -> std::io::Result<()> {
        codex_plus_plus::save_auto_auth(self, auth, guard)
    }

    fn delete_guarded(&self, guard: &AuthRefreshGuard) -> std::io::Result<bool> {
        codex_plus_plus::delete_auto_auth(self, guard)
    }
}

impl AuthStorageBackend for AutoAuthStorage {
    fn persistent_home(&self) -> Option<&Path> {
        Some(&self.file_storage.codex_home)
    }

    fn load_account_grant(&self) -> std::io::Result<Option<AuthDotJson>> {
        codex_plus_plus::load_auto_account_grant(self)
    }

    fn compare_and_save(&self, expected: &AuthDotJson, auth: &AuthDotJson) -> std::io::Result<()> {
        let guard = AuthRefreshGuard::acquire(&self.codex_home)?;
        self.compare_and_save_with_guard(expected, auth, &guard)
    }

    fn compare_and_save_with_guard(
        &self,
        expected: &AuthDotJson,
        auth: &AuthDotJson,
        guard: &AuthRefreshGuard,
    ) -> std::io::Result<()> {
        guard.ensure_matches(&self.codex_home)?;
        codex_plus_plus::auto_auth::compare_and_save(self, expected, auth, guard)
    }

    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        let guard = AuthRefreshGuard::acquire(&self.codex_home)?;
        self.load_with_guard(&guard)
    }

    fn load_with_guard(&self, guard: &AuthRefreshGuard) -> std::io::Result<Option<AuthDotJson>> {
        guard.ensure_matches(&self.codex_home)?;
        self.load_guarded(guard)
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        let guard = AuthRefreshGuard::acquire(&self.codex_home)?;
        self.save_with_guard(auth, &guard)
    }

    fn save_with_guard(&self, auth: &AuthDotJson, guard: &AuthRefreshGuard) -> std::io::Result<()> {
        guard.ensure_matches(&self.codex_home)?;
        self.save_guarded(auth, guard)
    }

    fn delete(&self) -> std::io::Result<bool> {
        let guard = AuthRefreshGuard::acquire(&self.codex_home)?;
        self.delete_with_guard(&guard)
    }

    fn delete_with_guard(&self, guard: &AuthRefreshGuard) -> std::io::Result<bool> {
        guard.ensure_matches(&self.codex_home)?;
        self.delete_guarded(guard)
    }
}

// A global in-memory store for mapping codex_home -> AuthDotJson.
static EPHEMERAL_AUTH_STORE: Lazy<Mutex<HashMap<String, AuthDotJson>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Debug)]
struct EphemeralAuthStorage {
    codex_home: PathBuf,
}

impl EphemeralAuthStorage {
    fn new(codex_home: PathBuf) -> Self {
        Self { codex_home }
    }

    fn with_store<F, T>(&self, action: F) -> std::io::Result<T>
    where
        F: FnOnce(&mut HashMap<String, AuthDotJson>, String) -> std::io::Result<T>,
    {
        let key = compute_store_key(&self.codex_home)?;
        let mut store = EPHEMERAL_AUTH_STORE
            .lock()
            .map_err(|_| std::io::Error::other("failed to lock ephemeral auth storage"))?;
        action(&mut store, key)
    }
}

impl AuthStorageBackend for EphemeralAuthStorage {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        self.with_store(|store, key| Ok(store.get(&key).cloned()))
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        self.with_store(|store, key| {
            store.insert(key, auth.clone());
            Ok(())
        })
    }

    fn delete(&self) -> std::io::Result<bool> {
        self.with_store(|store, key| Ok(store.remove(&key).is_some()))
    }
}

pub(super) fn create_auth_storage(
    codex_home: PathBuf,
    mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> Arc<dyn AuthStorageBackend> {
    let keyring_store: Arc<dyn KeyringStore> = Arc::new(DefaultKeyringStore);
    create_auth_storage_with_store(codex_home, mode, keyring_store, keyring_backend_kind)
}

pub(crate) fn save_file_auth_if_unchanged(
    codex_home: &Path,
    expected: &AuthDotJson,
    auth: &AuthDotJson,
    guard: &AuthRefreshGuard,
) -> std::io::Result<()> {
    guard.ensure_matches(codex_home)?;
    storage_commit::codex_plus_plus::save_file_if_unchanged(codex_home, expected, auth, guard)
}

pub(super) fn create_auth_storage_with_store(
    codex_home: PathBuf,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> Arc<dyn AuthStorageBackend> {
    let commit_home = codex_home.clone();
    let storage: Arc<dyn AuthStorageBackend> = match mode {
        AuthCredentialsStoreMode::File => Arc::new(FileAuthStorage::new(codex_home)),
        AuthCredentialsStoreMode::Keyring => {
            create_keyring_auth_storage(codex_home, keyring_store, keyring_backend_kind, mode)
        }
        AuthCredentialsStoreMode::Auto => Arc::new(AutoAuthStorage::new(
            codex_home,
            keyring_store,
            keyring_backend_kind,
        )),
        AuthCredentialsStoreMode::Ephemeral => Arc::new(EphemeralAuthStorage::new(codex_home)),
    };
    storage_commit::wrap(
        storage_telemetry::observe(storage, mode, keyring_backend_kind),
        commit_home,
        mode,
    )
}

fn create_keyring_auth_storage(
    codex_home: PathBuf,
    keyring_store: Arc<dyn KeyringStore>,
    keyring_backend_kind: AuthKeyringBackendKind,
    mode: AuthCredentialsStoreMode,
) -> Arc<dyn AuthStorageBackend> {
    match keyring_backend_kind {
        AuthKeyringBackendKind::Direct => Arc::new(DirectKeyringAuthStorage::new(
            codex_home,
            keyring_store,
            mode,
        )),
        AuthKeyringBackendKind::Secrets => Arc::new(SecretsKeyringAuthStorage::new(
            codex_home,
            keyring_store,
            mode,
        )),
    }
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
