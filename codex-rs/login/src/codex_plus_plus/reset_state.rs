use super::AccountId;
use super::AccountStore;
use super::replace_file;
use crate::CodexAuth;
use crate::account_lease::AccountLease;
use rand::RngCore as _;
use serde::Deserialize;
use serde::Serialize;
use std::fs::File;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

const STATE_FILE: &str = "rate-limit-reset-state.json";
const LOCK_FILE: &str = "rate-limit-reset.lock";
const MAX_STATE_BYTES: u64 = 4 * 1024;
const RESET_LEASE_POLL_INTERVAL: Duration = Duration::from_millis(25);
const STATE_VERSION: u8 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ResetAttemptPhase {
    Redeeming {
        credit_id: String,
        redeem_request_id: String,
    },
    ActivatingWeekly,
    ManualRedeeming {
        redeem_request_id: String,
        credit_id: Option<String>,
        source: ResetCredentialSource,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResetCompletion {
    pub id: String,
    pub completed_at: i64,
    #[serde(default)]
    pub manual: bool,
    /// Native source of an owned-backend completion; the account is bound by this state file.
    pub source: Option<ResetCredentialSource>,
    #[serde(default)]
    pub reconciliation: ResetReconciliation,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetReconciliation {
    #[default]
    Pending,
    DispatchedUnknown,
    Acknowledged,
    ObservedClear,
}

impl ResetCompletion {
    pub fn needs_proxy_reconciliation(&self) -> bool {
        self.source.is_some()
            && matches!(
                self.reconciliation,
                ResetReconciliation::Pending | ResetReconciliation::DispatchedUnknown
            )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetCredentialSource {
    Root,
    Imported,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManualResetAttempt {
    Fresh,
    Pending {
        redeem_request_id: String,
        credit_id: Option<String>,
    },
    Completed,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResetState {
    pub phase: Option<ResetAttemptPhase>,
    pub completion: Option<ResetCompletion>,
}

pub struct ResetMutationLease {
    state_path: PathBuf,
    _lease: AccountLease,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedState {
    version: u8,
    phase: Option<ResetAttemptPhase>,
    completion: Option<ResetCompletion>,
}

impl PersistedState {
    fn new() -> Self {
        Self {
            version: STATE_VERSION,
            phase: None,
            completion: None,
        }
    }
}

impl AccountStore {
    /// Enumerate existing completions, including root manual resets without an imported profile.
    pub fn pending_proxy_resets(&self) -> io::Result<Vec<(AccountId, ResetCompletion)>> {
        let entries = match std::fs::read_dir(self.accounts_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut pending = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() || !entry.path().join(STATE_FILE).is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let account = AccountId(name);
            if let Some(lease) = self.try_acquire_reset_mutation_lease(&account)? {
                let state = lease.state()?;
                if matches!(
                    state.phase,
                    None | Some(ResetAttemptPhase::ActivatingWeekly)
                ) && let Some(completion) = state
                    .completion
                    .filter(ResetCompletion::needs_proxy_reconciliation)
                {
                    pending.push((account, completion));
                }
            }
        }
        Ok(pending)
    }

    pub async fn acquire_reset_mutation_lease_for_auth(
        &self,
        auth: &CodexAuth,
        deadline: Instant,
    ) -> io::Result<Option<ResetMutationLease>> {
        let account_id = match auth {
            CodexAuth::Chatgpt(_) | CodexAuth::ChatgptAuthTokens(_) => {
                let tokens = auth.get_token_data()?;
                match super::account_id_for_token_data(&tokens) {
                    Ok(account_id) => account_id,
                    Err(err) if err.kind() == io::ErrorKind::InvalidInput => return Ok(None),
                    Err(err) => return Err(err),
                }
            }
            CodexAuth::AgentIdentity(auth) => super::account_id_for_workspace(auth.account_id()),
            CodexAuth::PersonalAccessToken(auth) => {
                super::account_id_for_workspace(auth.account_id())
            }
            CodexAuth::ApiKey(_)
            | CodexAuth::Headers(_)
            | CodexAuth::BedrockApiKey(_)
            | CodexAuth::BedrockAccessKeys(_) => {
                return Ok(None);
            }
        };
        loop {
            let store = self.clone();
            let account_id = account_id.clone();
            let lease = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                tokio::task::spawn_blocking(move || {
                    store.try_acquire_reset_mutation_lease(&account_id)
                }),
            )
            .await
            .map_err(|_| reset_lease_timeout())?
            .map_err(|err| {
                io::Error::other(format!("failed to join reset lease lookup: {err}"))
            })??;
            if let Some(lease) = lease {
                if Instant::now() < deadline {
                    return Ok(Some(lease));
                }
                drop(lease);
                return Err(reset_lease_timeout());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(reset_lease_timeout());
            }
            tokio::time::sleep((deadline - now).min(RESET_LEASE_POLL_INTERVAL)).await;
        }
    }

    pub fn acquire_reset_mutation_lease(
        &self,
        account_id: &AccountId,
    ) -> io::Result<ResetMutationLease> {
        let account_home = self.account_home(account_id);
        let lease = AccountLease::acquire(&account_home.join(LOCK_FILE))?;
        Ok(ResetMutationLease {
            state_path: account_home.join(STATE_FILE),
            _lease: lease,
        })
    }

    pub fn try_acquire_reset_mutation_lease(
        &self,
        account_id: &AccountId,
    ) -> io::Result<Option<ResetMutationLease>> {
        self.try_acquire_reset_mutation_lease_for_home(&self.account_home(account_id))
    }

    pub(crate) fn try_acquire_reset_mutation_lease_for_home(
        &self,
        account_home: &Path,
    ) -> io::Result<Option<ResetMutationLease>> {
        AccountLease::try_acquire(&account_home.join(LOCK_FILE)).map(|lease| {
            lease.map(|_lease| ResetMutationLease {
                state_path: account_home.join(STATE_FILE),
                _lease,
            })
        })
    }
}

impl ResetMutationLease {
    pub fn state(&self) -> io::Result<ResetState> {
        read_state(&self.state_path).map(|state| ResetState {
            phase: state.phase,
            completion: state.completion,
        })
    }

    pub fn load_or_begin(&mut self, credit_id: &str) -> io::Result<ResetAttemptPhase> {
        if credit_id.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reset credit id must not be empty",
            ));
        }
        let mut state = read_state(&self.state_path)?;
        if let Some(phase) = state.phase {
            return Ok(phase);
        }
        let phase = ResetAttemptPhase::Redeeming {
            credit_id: credit_id.to_string(),
            redeem_request_id: fresh_request_id(),
        };
        state.phase = Some(phase.clone());
        write_state(&self.state_path, &state)?;
        Ok(phase)
    }

    pub fn confirm_redeemed(
        &mut self,
        redeem_request_id: &str,
        completed_at: i64,
        source: Option<ResetCredentialSource>,
    ) -> io::Result<bool> {
        let mut state = read_state(&self.state_path)?;
        let Some(id) = state.phase.as_ref().and_then(|phase| match phase {
            ResetAttemptPhase::Redeeming {
                redeem_request_id: current,
                ..
            } if current == redeem_request_id => Some(current.clone()),
            ResetAttemptPhase::Redeeming { .. }
            | ResetAttemptPhase::ManualRedeeming { .. }
            | ResetAttemptPhase::ActivatingWeekly => None,
        }) else {
            return Ok(false);
        };
        state.phase = Some(ResetAttemptPhase::ActivatingWeekly);
        state.completion = Some(ResetCompletion {
            id,
            completed_at,
            manual: false,
            source,
            reconciliation: ResetReconciliation::Pending,
        });
        write_state(&self.state_path, &state)?;
        Ok(true)
    }

    pub fn clear_redeeming(&mut self, redeem_request_id: &str) -> io::Result<bool> {
        let mut state = read_state(&self.state_path)?;
        let matches = matches!(
            state.phase.as_ref(),
            Some(ResetAttemptPhase::Redeeming {
                redeem_request_id: current,
                ..
            } | ResetAttemptPhase::ManualRedeeming {
                redeem_request_id: current,
                ..
            }) if current == redeem_request_id
        );
        if !matches {
            return Ok(false);
        }
        state.phase = None;
        write_state(&self.state_path, &state)?;
        Ok(true)
    }

    pub fn begin_manual(
        &mut self,
        request_id: &str,
        source: ResetCredentialSource,
        credit_id: Option<&str>,
    ) -> io::Result<ManualResetAttempt> {
        let mut state = read_state(&self.state_path)?;
        match state.phase.as_ref() {
            Some(ResetAttemptPhase::ManualRedeeming {
                redeem_request_id,
                credit_id,
                source: current,
            }) if *current == source => {
                return Ok(ManualResetAttempt::Pending {
                    redeem_request_id: redeem_request_id.clone(),
                    credit_id: credit_id.clone(),
                });
            }
            Some(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "another usage reset is pending",
                ));
            }
            None => {}
        }
        if let Some(done) = &state.completion
            && done.id == request_id
        {
            if done.manual && done.source == Some(source) {
                return Ok(ManualResetAttempt::Completed);
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reset request belongs to a different account source",
            ));
        }
        state.phase = Some(ResetAttemptPhase::ManualRedeeming {
            redeem_request_id: request_id.to_owned(),
            credit_id: credit_id.map(str::to_owned),
            source,
        });
        write_state(&self.state_path, &state)?;
        Ok(ManualResetAttempt::Fresh)
    }

    pub fn confirm_manual(&mut self, request_id: &str, completed_at: i64) -> io::Result<bool> {
        let mut state = read_state(&self.state_path)?;
        let Some(ResetAttemptPhase::ManualRedeeming {
            redeem_request_id,
            source,
            ..
        }) = state.phase
        else {
            return Ok(false);
        };
        if redeem_request_id != request_id {
            return Ok(false);
        }
        state.phase = None;
        state.completion = Some(ResetCompletion {
            id: redeem_request_id,
            completed_at,
            manual: true,
            source: Some(source),
            reconciliation: ResetReconciliation::Pending,
        });
        write_state(&self.state_path, &state)?;
        Ok(true)
    }

    /// Compare the complete receipt before writing; an old attempt cannot acknowledge a successor.
    pub fn reconcile_proxy(
        &mut self,
        expected: &ResetCompletion,
        next: ResetReconciliation,
    ) -> io::Result<bool> {
        let mut state = read_state(&self.state_path)?;
        if state.completion.as_ref() != Some(expected)
            || !matches!(
                state.phase,
                None | Some(ResetAttemptPhase::ActivatingWeekly)
            )
            || expected.source.is_none()
            || !matches!(
                (expected.reconciliation, next),
                (
                    ResetReconciliation::Pending,
                    ResetReconciliation::DispatchedUnknown | ResetReconciliation::ObservedClear
                ) | (
                    ResetReconciliation::DispatchedUnknown,
                    ResetReconciliation::Acknowledged | ResetReconciliation::ObservedClear
                )
            )
        {
            return Ok(false);
        }
        let mut completion = expected.clone();
        completion.reconciliation = next;
        state.completion = Some(completion);
        write_state(&self.state_path, &state)?;
        Ok(true)
    }

    pub fn finish_weekly_activation(&mut self) -> io::Result<bool> {
        let mut state = read_state(&self.state_path)?;
        if state.phase != Some(ResetAttemptPhase::ActivatingWeekly) {
            return Ok(false);
        }
        state.phase = None;
        write_state(&self.state_path, &state)?;
        Ok(true)
    }
}

fn fresh_request_id() -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut id = String::with_capacity(36);
    for (index, byte) in bytes.into_iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            id.push('-');
        }
        id.push(char::from(HEX[usize::from(byte >> 4)]));
        id.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    id
}

fn read_state(path: &Path) -> io::Result<PersistedState> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(PersistedState::new()),
        Err(err) => return Err(err),
    };
    let mut bytes = Vec::new();
    Read::take(file, MAX_STATE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(invalid_state("rate limit reset state is too large"));
    }
    let state: PersistedState = serde_json::from_slice(&bytes)
        .map_err(|err| invalid_state(format!("invalid rate limit reset state: {err}")))?;
    if state.version != STATE_VERSION {
        let version = state.version;
        return Err(invalid_state(format!(
            "unsupported rate limit reset state version {version}"
        )));
    }
    Ok(state)
}

fn write_state(path: &Path, state: &PersistedState) -> io::Result<()> {
    let bytes = serde_json::to_vec(state).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(io::Error::other("rate limit reset state is too large"));
    }
    let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    std::fs::create_dir_all(parent)?;
    let temp_path = parent.join("rate-limit-reset-state.json.tmp");
    let mut file = File::create(&temp_path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    replace_file(&temp_path, path)?;
    #[cfg(not(windows))]
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn invalid_state(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn reset_lease_timeout() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "rate limit reset lease acquisition timed out",
    )
}

#[cfg(test)]
#[path = "reset_state_tests.rs"]
mod tests;
