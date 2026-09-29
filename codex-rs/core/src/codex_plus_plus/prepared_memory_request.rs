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
                    redirect_policy: ClientRedirectPolicy::Reject,
                    api_auth: prepared.auth.auth,
                    agent_identity_telemetry: prepared.auth.agent_identity_telemetry,
                },
                ReqwestTransport::from_http_client(client),
                prepared.model,
            ));
        }
        let setup = self
            .current_client_setup(ClientRouting::ConfiguredProvider)
            .await?;
        let transport = self.build_api_transport(
            &setup.api_provider,
            MEMORIES_SUMMARIZE_ENDPOINT,
            setup.redirect_policy,
        )?;
        Ok((setup, transport, model.to_owned()))
    }
}
