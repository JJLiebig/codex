//! Use a captured owned-provider route while preserving native memory request setup.

use super::*;

impl ModelClient {
    pub(super) async fn memory_request_setup(
        &self,
        model: &str,
    ) -> Result<(CurrentClientSetup, ReqwestTransport, String)> {
        if let Some(prepared) = self.state.provider.prepare_request(model).await? {
            let client = prepared.http_client.ok_or_else(|| {
                CodexErr::UnsupportedOperation("Prepared request has no HTTP client".into())
            })?;
            return Ok((
                CurrentClientSetup {
                    auth: None,
                    auth_owner_generation: None,
                    auth_revision: prepared.route.as_ref().map(|route| route.auth_revision),
                    api_provider: prepared.provider,
                    http_client_factory: self.http_client_factory.clone(),
                    api_http_client: None,
                    redirect_policy: ClientRedirectPolicy::Reject,
                    api_auth: prepared.auth.auth,
                    agent_identity_telemetry: prepared.auth.agent_identity_telemetry,
                    runtime_id: None,
                },
                ReqwestTransport::from_http_client(client),
                prepared.model,
            ));
        }
        let setup = self
            .current_client_setup(ClientRouting::ConfiguredProvider)
            .await?;
        let transport = setup.build_api_transport(MEMORIES_SUMMARIZE_ENDPOINT)?;
        Ok((setup, transport, model.to_owned()))
    }
}
