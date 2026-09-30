use super::*;
use codex_app_server_protocol::UsageResetCompletion;
use codex_app_server_protocol::UsageResetTargetParams;
use codex_login::AccountId;
use codex_login::AccountStore;
use codex_login::NativeCredentialSource;
use codex_login::ResetCredentialSource;
use codex_protocol::inference_attribution::InferenceNativeSource;

pub(super) async fn read(
    processor: &AccountRequestProcessor,
    target: UsageResetTargetParams,
) -> Result<GetAccountRateLimitsResponse, JSONRPCErrorError> {
    tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        let account_id: AccountId = serde_json::from_value(serde_json::json!(target.account_id))
            .map_err(|error| invalid_request(format!("invalid reset account: {error}")))?;
        let source = match target.source {
            InferenceNativeSource::Root => NativeCredentialSource::Root(account_id.clone()),
            InferenceNativeSource::Imported => NativeCredentialSource::Imported(account_id.clone()),
        };
        let client = {
            let snapshot = processor
                .auth_manager
                .export_native_credentials()
                .await
                .map_err(|error| internal_error(error.to_string()))?;
            let credential = snapshot
                .credentials()
                .iter()
                .find(|credential| credential.source == source)
                .ok_or_else(|| invalid_request("reset account is unavailable"))?;
            let auth = codex_login::CodexAuth::from_external_chatgpt_tokens(
                &credential.access_token,
                &credential.upstream_account_id,
                credential.plan_type.as_deref(),
            )
            .map_err(|error| internal_error(error.to_string()))?;
            BackendClient::from_auth(
                &processor.config.chatgpt_base_url,
                &auth,
                processor.config.http_client_factory(),
            )
        };
        let store = AccountStore::new(processor.config.codex_home.to_path_buf());
        let completion = {
            let lease = store
                .try_acquire_reset_mutation_lease(&account_id)
                .map_err(|error| internal_error(error.to_string()))?
                .ok_or_else(|| invalid_request("reset account is busy"))?;
            let state = lease
                .state()
                .map_err(|error| internal_error(error.to_string()))?;
            state
                .completion
                .filter(|completion| {
                    state.phase.is_none()
                        && completion.source
                            == Some(match target.source {
                                InferenceNativeSource::Root => ResetCredentialSource::Root,
                                InferenceNativeSource::Imported => ResetCredentialSource::Imported,
                            })
                        && completion.completed_at / 1_000_000_000 >= target.failed_at
                        && target
                            .completion_id
                            .as_ref()
                            .is_none_or(|id| id == &completion.id)
                })
                .ok_or_else(|| invalid_request("no matching completed reset"))?
        };
        let usage = client
            .get_rate_limits_with_reset_credits()
            .await
            .map_err(|error| internal_error(error.to_string()))?;
        let ready = codex_model_provider::cli_proxy_reset_ready(
            &processor.config.codex_home,
            processor.config.http_client_factory(),
            &account_id,
            &completion,
            usage.account_id.as_deref(),
            &usage.rate_limits,
            usage.ordinary_usage_allowed,
        )
        .await
        .map_err(|error| internal_error(error.to_string()))?;
        let rate_limits = usage
            .rate_limits
            .iter()
            .find(|snapshot| snapshot.limit_id.as_deref() == Some("codex"))
            .ok_or_else(|| internal_error("missing native quota"))?
            .clone()
            .into();
        Ok(GetAccountRateLimitsResponse {
            reset_admission: ready.then_some(UsageResetCompletion {
                id: completion.id,
                source: target.source,
                account_id: target.account_id,
                completed_at: completion.completed_at / 1_000_000_000,
            }),
            ordinary_usage_allowed: usage.ordinary_usage_allowed,
            rate_limits,
            rate_limits_by_limit_id: Some(
                usage
                    .rate_limits
                    .into_iter()
                    .filter_map(|limit| Some((limit.limit_id.clone()?, limit.into())))
                    .collect(),
            ),
            rate_limit_reset_credits: None,
            account_id: usage.account_id,
            rate_limit_upsell: None,
        })
    })
    .await
    .map_err(|_| internal_error("reset admission timed out"))?
}
