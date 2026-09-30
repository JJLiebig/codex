//! Captured owned HTTP routing with terminal recovery until source-bound failover is enabled.
use super::*;
use codex_login::NativeCredentialSource;
use codex_model_provider::ProxyRequestRoute;

pub(super) struct OwnedRequest {
    route: Option<ProxyRequestRoute>,
    pub(super) response_trace: OnceLock<Option<String>>,
    pub(super) served_native_source: OnceLock<Option<NativeCredentialSource>>,
    accepted_output: AtomicBool,
}

pub(super) fn observe_stream(
    stream: codex_api::ResponseStream,
    request: Option<Arc<OwnedRequest>>,
) -> codex_extension_api::ModelResponseStream {
    if let Some(request) = &request {
        request.set_trace(stream.proxy_trace_id.as_deref());
    }
    Box::pin(stream.inspect(move |event| {
        if let Some(request) = &request {
            request.observe(event);
        }
    }))
}

impl OwnedRequest {
    pub(super) fn set_trace(&self, trace: Option<&str>) {
        let trace = trace.filter(|trace| trace.len() <= 128);
        let source = trace.and_then(|trace| {
            let parts: Vec<_> = trace.split('-').collect();
            let route = self.route.as_ref()?;
            (parts.len() == 3
                && parts[0].len() == 14
                && parts[0].bytes().all(|byte| byte.is_ascii_digit())
                && parts[1].len() == 16
                && parts[1].bytes().all(|byte| byte.is_ascii_hexdigit())
                && parts[2].len() == 8
                && parts[2].bytes().all(|byte| byte.is_ascii_hexdigit())
                && route.native_auth_index() == Some(parts[1]))
            .then(|| route.native_source().cloned())
            .flatten()
        });
        let _ = self.response_trace.set(trace.map(str::to_owned));
        let _ = self.served_native_source.set(source);
    }

    pub(super) fn observe(&self, event: &std::result::Result<ResponseEvent, ApiError>) {
        if matches!(
            event,
            Ok(ResponseEvent::OutputItemAdded(_)
                | ResponseEvent::OutputItemDone(_)
                | ResponseEvent::OutputTextDelta(_)
                | ResponseEvent::ToolCallInputDelta { .. }
                | ResponseEvent::ReasoningSummaryDelta { .. }
                | ResponseEvent::ReasoningSummaryDone { .. }
                | ResponseEvent::ReasoningContentDelta { .. }
                | ResponseEvent::ReasoningSummaryPartAdded { .. }
                | ResponseEvent::Completed { .. })
        ) {
            self.accepted_output.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn map_error(&self, provider: &SharedModelProvider, error: ApiError) -> CodexErr {
        let status = if let ApiError::Transport(TransportError::Http {
            status, headers, ..
        }) = &error
        {
            self.set_trace(
                headers
                    .as_ref()
                    .and_then(|headers| headers.get("x-cpa-trace-id"))
                    .and_then(|trace| trace.to_str().ok()),
            );
            Some(*status)
        } else {
            None
        };
        let mapped = provider.map_api_error(error);
        let mapped = if status == Some(StatusCode::UNAUTHORIZED) {
            CodexErr::UnsupportedOperation("The model provider rejected authentication.".into())
        } else if status == Some(StatusCode::TOO_MANY_REQUESTS)
            || matches!(
                mapped.details(),
                CodexErrorDetails::UsageLimitReached(_)
                    | CodexErrorDetails::QuotaExceeded
                    | CodexErrorDetails::UsageNotIncluded
            )
        {
            // Native recovery requires a guarded source expectation; never use the current login.
            CodexErr::UnsupportedOperation("The model provider reached its usage limit.".into())
        } else {
            mapped
        };
        if self.accepted_output.load(Ordering::Relaxed) {
            let reason = match mapped.details() {
                CodexErrorDetails::UnsupportedOperation(reason) => reason.clone(),
                _ => mapped.to_string(),
            };
            return CodexErr::UnsupportedOperation(format!(
                "Model response interrupted after output: {reason}"
            ));
        }
        mapped
    }
}

impl ModelClientSession {
    pub(super) async fn response_request_setup(
        &mut self,
        model: &str,
    ) -> Result<(CurrentClientSetup, ReqwestTransport, String)> {
        self.owned_request = None;
        if let Some(mut prepared) = self.client.state.provider.prepare_request(model).await? {
            // Let the owning turn retry so every HTTP attempt gets a fresh publication snapshot.
            prepared.provider.retry.max_attempts = 0;
            let transport =
                ReqwestTransport::from_http_client(prepared.http_client.ok_or_else(|| {
                    CodexErr::UnsupportedOperation("Prepared request has no HTTP client".into())
                })?);
            let revision = prepared.route.as_ref().map(|route| route.auth_revision);
            self.owned_request = Some(Arc::new(OwnedRequest {
                route: prepared.route,
                response_trace: OnceLock::new(),
                served_native_source: OnceLock::new(),
                accepted_output: AtomicBool::new(false),
            }));
            return Ok((
                CurrentClientSetup {
                    auth: None,
                    auth_owner_generation: None,
                    auth_revision: revision,
                    api_provider: prepared.provider,
                    redirect_policy: ClientRedirectPolicy::Reject,
                    api_auth: prepared.auth.auth,
                    agent_identity_telemetry: prepared.auth.agent_identity_telemetry,
                },
                transport,
                prepared.model,
            ));
        }
        let setup = self
            .client
            .current_client_setup(ClientRouting::Workspace)
            .await?;
        let transport = self.client.build_api_transport(
            &setup.api_provider,
            "/responses",
            setup.redirect_policy,
        )?;
        Ok((setup, transport, model.to_owned()))
    }

    pub(crate) fn owned_retry_forbidden(&self, error: &CodexErr) -> bool {
        self.owned_request.as_ref().is_some_and(|request| {
            request.accepted_output.load(Ordering::Relaxed)
                || error.retry_delay(/*retry_count*/ 1).is_none()
        })
    }
}
