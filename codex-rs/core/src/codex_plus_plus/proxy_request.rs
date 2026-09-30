//! Captured owned HTTP routing and source-bound, pre-output native quota failover.
use super::*;
use codex_login::NativeCredentialSource;
use codex_model_provider::ProxyRequestRoute;
use codex_protocol::inference_attribution::InferenceAttribution;
use codex_protocol::inference_attribution::InferenceNativeSource;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum NativeQuotaAttribution {
    Served,
    Intended,
}

pub(super) struct OwnedRequest {
    route: Option<ProxyRequestRoute>,
    wire_model: String,
    pub(super) quota_attribution: OnceLock<NativeQuotaAttribution>,
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
    fn failure_attribution(&self) -> InferenceAttribution {
        let served = self.served_native_source.get().and_then(Option::as_ref);
        let intended = (self.quota_attribution.get() == Some(&NativeQuotaAttribution::Intended))
            .then(|| {
                self.route
                    .as_ref()
                    .and_then(ProxyRequestRoute::native_source)
            })
            .flatten();
        if let Some(native) = served.or(intended) {
            let (source, id) = match native {
                NativeCredentialSource::Root(id) => (InferenceNativeSource::Root, id),
                NativeCredentialSource::Imported(id) => (InferenceNativeSource::Imported, id),
            };
            let account_id = id.to_string();
            if served.is_some() {
                InferenceAttribution::ServedNative { source, account_id }
            } else {
                InferenceAttribution::IntendedNative { source, account_id }
            }
        } else if self
            .route
            .as_ref()
            .is_some_and(|route| route.is_claude_model(&self.wire_model))
        {
            InferenceAttribution::Claude
        } else {
            InferenceAttribution::Unknown
        }
    }

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
        let mut cooldown = None;
        let status = if let ApiError::Transport(TransportError::Http {
            status,
            headers,
            body,
            ..
        }) = &error
        {
            self.set_trace(
                headers
                    .as_ref()
                    .and_then(|headers| headers.get("x-cpa-trace-id"))
                    .and_then(|trace| trace.to_str().ok()),
            );
            if *status == StatusCode::TOO_MANY_REQUESTS
                && !headers
                    .as_ref()
                    .is_some_and(|headers| headers.contains_key("x-cpa-trace-id"))
                && let Some(body) = body
                && let Ok(value) = serde_json::from_str::<serde_json::Value>(body)
                && let Some(error) = value.get("error")
                && error.get("code").and_then(serde_json::Value::as_str) == Some("model_cooldown")
                && error.get("model").and_then(serde_json::Value::as_str)
                    == Some(self.wire_model.as_str())
            {
                // The proxy made no upstream attempt. Never infer identity from nested errors.
                cooldown = Some(codex_api::map_api_error(ApiError::UsageLimitReached {
                    plan_type: None,
                    resets_at: error
                        .get("reset_seconds")
                        .and_then(serde_json::Value::as_i64)
                        .filter(|seconds| *seconds >= 0)
                        .and_then(|seconds| chrono::Utc::now().timestamp().checked_add(seconds)),
                    limit_window_minutes: None,
                }));
            }
            Some(*status)
        } else {
            None
        };
        let intended = cooldown.is_some();
        let mapped = cooldown.unwrap_or_else(|| provider.map_api_error(error));
        if matches!(mapped.details(), CodexErrorDetails::UsageLimitReached(_))
            && self
                .route
                .as_ref()
                .and_then(ProxyRequestRoute::native_expectation)
                .is_some()
        {
            let attribution = if intended {
                Some(NativeQuotaAttribution::Intended)
            } else {
                self.served_native_source
                    .get()
                    .and_then(Option::as_ref)
                    .map(|_| NativeQuotaAttribution::Served)
            };
            if let Some(attribution) = attribution {
                let _ = self.quota_attribution.set(attribution);
            }
        }
        let mapped = if status == Some(StatusCode::UNAUTHORIZED) {
            CodexErr::UnsupportedOperation("The model provider rejected authentication.".into())
        } else if self.quota_attribution.get().is_none()
            && (status == Some(StatusCode::TOO_MANY_REQUESTS)
                || matches!(
                    mapped.details(),
                    CodexErrorDetails::UsageLimitReached(_)
                        | CodexErrorDetails::QuotaExceeded
                        | CodexErrorDetails::UsageNotIncluded
                ))
        {
            CodexErr::UnsupportedOperation("The model provider reached its usage limit.".into())
        } else {
            mapped
        };
        let mapped = if self.accepted_output.load(Ordering::Relaxed) {
            let reason = match mapped.details() {
                CodexErrorDetails::UnsupportedOperation(reason) => reason.clone(),
                _ => mapped.to_string(),
            };
            CodexErr::UnsupportedOperation(format!(
                "Model response interrupted after output: {reason}"
            ))
        } else {
            mapped
        };
        mapped.with_inference_attribution(self.failure_attribution())
    }
}

impl ModelClientSession {
    pub(crate) fn attribute_owned_error(&self, error: CodexErr) -> CodexErr {
        match &self.owned_request {
            Some(request) => error.with_inference_attribution(request.failure_attribution()),
            None => error,
        }
    }

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
                wire_model: prepared.model.clone(),
                quota_attribution: OnceLock::new(),
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

    pub(crate) fn owned_quota_attribution(&self) -> Option<NativeQuotaAttribution> {
        self.owned_request
            .as_ref()?
            .quota_attribution
            .get()
            .copied()
    }

    pub(crate) async fn switch_owned_quota(
        &mut self,
        attempted: &mut HashSet<String>,
        usage: &codex_protocol::error::UsageLimitReachedError,
    ) -> Result<Option<ImportedAccountSwitchOutcome>> {
        let Some(request) = self.owned_request.as_ref() else {
            return Ok(None);
        };
        let changed = || {
            CodexErr::UnsupportedOperation(
                "The account changed while the request was running. Try again.".into(),
            )
        };
        if request.accepted_output.load(Ordering::Relaxed)
            || request.quota_attribution.get().is_none()
        {
            return Err(CodexErr::UnsupportedOperation(
                "The model provider reached its usage limit.".into(),
            ));
        }
        let expected = request
            .route
            .as_ref()
            .and_then(ProxyRequestRoute::native_expectation)
            .ok_or_else(changed)?;
        let manager = self
            .client
            .state
            .provider
            .auth_manager()
            .ok_or_else(changed)?;
        let outcome = manager
            .switch_after_native_usage_limit(
                expected,
                attempted,
                usage.resets_at.map(|time| time.timestamp()),
            )
            .await
            .map_err(|_| changed())?;
        self.usage_limit_failover_tracking
            .attempted_account_ids
            .extend(attempted.iter().cloned());
        match outcome {
            ImportedAccountSwitchOutcome::RequestSourceChanged => return Err(changed()),
            ImportedAccountSwitchOutcome::SelectedBlockedUntil { .. } => {
                return Err(CodexErr::UnsupportedOperation(
                    "The model provider reached its usage limit.".into(),
                ));
            }
            ImportedAccountSwitchOutcome::ReadyToRetry => {
                if let Some(id) = manager.active_account_id() {
                    self.usage_limit_failover_tracking
                        .selected_account_ids
                        .push(id);
                }
                self.reset_websocket_session();
            }
            ImportedAccountSwitchOutcome::NoCandidate => {}
        }
        Ok(Some(outcome))
    }

    pub(crate) fn owned_retry_forbidden(&self, error: &CodexErr) -> bool {
        self.owned_request.as_ref().is_some_and(|request| {
            request.accepted_output.load(Ordering::Relaxed)
                || matches!(error.details(), CodexErrorDetails::UnsupportedOperation(_))
        })
    }
}
