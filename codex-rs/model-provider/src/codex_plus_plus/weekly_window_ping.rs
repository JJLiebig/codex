use std::sync::Arc;
use std::time::Duration;

use codex_agent_identity::ChatGptEnvironment;
use codex_api::ApiError;
use codex_api::Compression;
use codex_api::ResponseEvent;
use codex_api::ResponsesClient;
use codex_api::RetryConfig;
use codex_api::TransportError;
use codex_http_client::HttpClientFactory;
use codex_http_client::HttpTransport;
use codex_http_client::OutboundProxyPolicy;
use codex_http_client::Request;
use codex_http_client::ReqwestTransport;
use codex_http_client::Response;
use codex_http_client::StreamResponse;
use codex_login::AuthConfig;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::default_client::create_client;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_models_manager::manager::ModelsManager as _;
use codex_models_manager::manager::OpenAiModelsManager;
use codex_models_manager::manager::RefreshStrategy;
use http::HeaderMap;
use serde_json::json;
use tokio::time::Instant;

use crate::auth::auth_provider_from_auth_manager;
use crate::models_endpoint::OpenAiModelsEndpoint;
use crate::provider::provider_uses_first_party_auth_path;

const PING_TIMEOUT: Duration = Duration::from_secs(30);

struct DeadlineTransport(ReqwestTransport, Instant);
impl HttpTransport for DeadlineTransport {
    async fn execute(&self, request: Request) -> Result<Response, TransportError> {
        self.0.execute(request).await
    }
    async fn stream(&self, mut request: Request) -> Result<StreamResponse, TransportError> {
        request.timeout = Some(self.1.saturating_duration_since(Instant::now()));
        self.0.stream(request).await
    }
}

pub struct WeeklyWindowPingRequest {
    pub auth_config: AuthConfig,
    pub model_provider_id: String,
    pub model_provider: ModelProviderInfo,
    pub chatgpt_base_url: String,
    pub http_client_factory: HttpClientFactory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WeeklyWindowPingOutcome {
    Completed,
    LocalSetup,
    Rejected { status: Option<u16> },
    Ambiguous { status: Option<u16> },
    LoginRequired,
    AuthenticationRecovery,
    UnsupportedConfiguration,
    UnsupportedRouting,
}

pub fn preflight_weekly_window_ping(
    model_provider_id: &str,
    model_provider: &ModelProviderInfo,
    chatgpt_base_url: &str,
    http_client_factory: &HttpClientFactory,
) -> Result<(), WeeklyWindowPingOutcome> {
    if model_provider_id != OPENAI_PROVIDER_ID
        || !model_provider.is_openai()
        || !provider_uses_first_party_auth_path(model_provider)
        || model_provider.base_url
            != ModelProviderInfo::create_openai_provider(/*base_url*/ None).base_url
    {
        return Err(WeeklyWindowPingOutcome::LocalSetup);
    }
    if chatgpt_base_url.trim_end_matches('/') != ChatGptEnvironment::default().chatgpt_base_url() {
        return Err(WeeklyWindowPingOutcome::UnsupportedConfiguration);
    }
    if http_client_factory.outbound_proxy_policy() != OutboundProxyPolicy::ReqwestDefault {
        return Err(WeeklyWindowPingOutcome::UnsupportedRouting);
    }
    Ok(())
}

pub async fn ping_weekly_window(request: WeeklyWindowPingRequest) -> WeeklyWindowPingOutcome {
    if let Err(outcome) = preflight_weekly_window_ping(
        &request.model_provider_id,
        &request.model_provider,
        &request.chatgpt_base_url,
        &request.http_client_factory,
    ) {
        return outcome;
    }

    ping_weekly_window_with_timeout(request, PING_TIMEOUT).await
}

async fn ping_weekly_window_with_timeout(
    request: WeeklyWindowPingRequest,
    timeout: Duration,
) -> WeeklyWindowPingOutcome {
    let deadline = Instant::now() + timeout;
    match tokio::time::timeout_at(deadline, ping_weekly_window_inner(request, deadline)).await {
        Ok(outcome) => outcome,
        Err(_) => WeeklyWindowPingOutcome::Ambiguous { status: None },
    }
}

async fn ping_weekly_window_inner(
    request: WeeklyWindowPingRequest,
    deadline: Instant,
) -> WeeklyWindowPingOutcome {
    let auth_manager = match AuthManager::new_from_file_auth(request.auth_config.clone()).await {
        Ok(Some(manager)) => Arc::new(manager),
        Ok(None) => return WeeklyWindowPingOutcome::LoginRequired,
        Err(_) => return WeeklyWindowPingOutcome::LocalSetup,
    };
    let Some(auth) = auth_manager.auth_cached() else {
        return WeeklyWindowPingOutcome::LoginRequired;
    };
    let root = request.chatgpt_base_url.trim_end_matches('/');
    let base_url = root.trim_end_matches("/codex");
    let provider_info =
        ModelProviderInfo::create_openai_provider(Some(format!("{base_url}/codex")));
    let model_manager = OpenAiModelsManager::new_without_cache(
        Arc::new(OpenAiModelsEndpoint::new(
            provider_info,
            Some(Arc::clone(&auth_manager)),
            /*gateway_auth_manager*/ None,
        )),
        Some(Arc::clone(&auth_manager)),
    );
    let model = model_manager
        .get_default_model(
            /*model*/ &None,
            /*allow_provider_model_fallback*/ false,
            RefreshStrategy::Online,
            request.http_client_factory.clone(),
        )
        .await;
    if model.is_empty() {
        return WeeklyWindowPingOutcome::LocalSetup;
    }
    let mut unauthorized_recovery = auth_manager.unauthorized_recovery();
    if let AttemptOutcome::Finished(outcome) =
        send_once(&request, &auth_manager, &auth, &model, deadline).await
    {
        return outcome;
    }
    let Ok(reload) = unauthorized_recovery.next().await else {
        return WeeklyWindowPingOutcome::AuthenticationRecovery;
    };
    if reload.auth_state_changed() != Some(true) {
        return WeeklyWindowPingOutcome::AuthenticationRecovery;
    }
    let Some(reloaded_auth) = auth_manager.auth_cached() else {
        return WeeklyWindowPingOutcome::AuthenticationRecovery;
    };
    if auth.get_account_id() != reloaded_auth.get_account_id()
        || auth.get_chatgpt_user_id() != reloaded_auth.get_chatgpt_user_id()
        || auth.is_workspace_account() != reloaded_auth.is_workspace_account()
    {
        return WeeklyWindowPingOutcome::AuthenticationRecovery;
    }
    match send_once(&request, &auth_manager, &reloaded_auth, &model, deadline).await {
        AttemptOutcome::Finished(outcome) => outcome,
        AttemptOutcome::Unauthorized => WeeklyWindowPingOutcome::AuthenticationRecovery,
    }
}

enum AttemptOutcome {
    Finished(WeeklyWindowPingOutcome),
    Unauthorized,
}

async fn send_once(
    request: &WeeklyWindowPingRequest,
    auth_manager: &Arc<AuthManager>,
    expected_auth: &CodexAuth,
    model: &str,
    deadline: Instant,
) -> AttemptOutcome {
    let root = request.chatgpt_base_url.trim_end_matches('/');
    let base_url = root.trim_end_matches("/codex");
    let provider_info =
        ModelProviderInfo::create_openai_provider(Some(format!("{base_url}/codex")));
    let Ok(mut provider) = provider_info.to_api_provider(Some(expected_auth.auth_mode())) else {
        return AttemptOutcome::Finished(WeeklyWindowPingOutcome::LocalSetup);
    };
    provider.retry = RetryConfig {
        max_attempts: 0,
        base_delay: Duration::ZERO,
        retry_429: false,
        retry_5xx: false,
        retry_transport: false,
    };
    let client = create_client();
    let auth = auth_provider_from_auth_manager(Arc::clone(auth_manager), expected_auth);
    let transport = DeadlineTransport(ReqwestTransport::from_http_client(client), deadline);
    let client = ResponsesClient::new(transport, provider, auth);
    let body = json!({
        "model": model,
        "instructions": "",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "Reply ACK only."}]
        }],
        "store": false,
        "stream": true,
        "include": []
    });
    let mut stream = match client
        .stream(
            body,
            HeaderMap::new(),
            Compression::None,
            /*turn_state*/ None,
        )
        .await
    {
        Ok(stream) => stream,
        Err(ApiError::Transport(TransportError::Http { status, .. }))
            if status == http::StatusCode::UNAUTHORIZED =>
        {
            return AttemptOutcome::Unauthorized;
        }
        Err(error) => return AttemptOutcome::Finished(classify_error(&error)),
    };

    while let Some(event) = stream.rx_event.recv().await {
        match event {
            Ok(ResponseEvent::Completed { .. }) => {
                return AttemptOutcome::Finished(WeeklyWindowPingOutcome::Completed);
            }
            Ok(_) => {}
            Err(error) => return AttemptOutcome::Finished(classify_error(&error)),
        }
    }
    AttemptOutcome::Finished(WeeklyWindowPingOutcome::Ambiguous { status: None })
}

fn classify_error(error: &ApiError) -> WeeklyWindowPingOutcome {
    match error {
        ApiError::Transport(TransportError::Http { status, .. }) | ApiError::Api { status, .. }
            if status.is_client_error() && *status != http::StatusCode::REQUEST_TIMEOUT =>
        {
            WeeklyWindowPingOutcome::Rejected {
                status: Some(status.as_u16()),
            }
        }
        ApiError::QuotaExceeded
        | ApiError::UsageLimitReached { .. }
        | ApiError::UsageNotIncluded
        | ApiError::RateLimitExceeded { .. }
        | ApiError::RateLimit(_)
        | ApiError::InvalidRequest { .. }
        | ApiError::CyberPolicy { .. }
        | ApiError::BioPolicy { .. }
        | ApiError::MisalignmentPolicyViolation { .. } => {
            WeeklyWindowPingOutcome::Rejected { status: None }
        }
        ApiError::Transport(TransportError::Http { status, .. }) | ApiError::Api { status, .. } => {
            WeeklyWindowPingOutcome::Ambiguous {
                status: Some(status.as_u16()),
            }
        }
        ApiError::Transport(_)
        | ApiError::Stream(_)
        | ApiError::Retryable { .. }
        | ApiError::ServerOverloaded
        | ApiError::ContextWindowExceeded => WeeklyWindowPingOutcome::Ambiguous { status: None },
    }
}

#[cfg(test)]
#[path = "weekly_window_ping_tests.rs"]
mod tests;
