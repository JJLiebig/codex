use std::path::PathBuf;

use codex_model_provider::WeeklyWindowPingRequest;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::OPENAI_PROVIDER_ID;

use crate::legacy_core::config::Config;

pub(super) fn ping_request(config: &Config, account_home: PathBuf) -> WeeklyWindowPingRequest {
    let mut auth_config = config.auth_config();
    auth_config.codex_home = account_home;
    WeeklyWindowPingRequest {
        auth_config,
        model_provider_id: OPENAI_PROVIDER_ID.to_string(),
        model_provider: ModelProviderInfo::create_openai_provider(/*base_url*/ None),
        chatgpt_base_url: config.chatgpt_base_url.clone(),
        http_client_factory: config.http_client_factory(),
    }
}
