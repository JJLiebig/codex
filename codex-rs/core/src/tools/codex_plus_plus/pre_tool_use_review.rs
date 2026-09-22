use crate::guardian::GuardianApprovalRequest;
use crate::guardian::new_guardian_review_id;
use crate::guardian::preflight_guardian_action;
use crate::guardian::review_pre_tool_use;
use crate::shell::ShellType;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::registry::PreToolUsePayload;
use codex_protocol::protocol::ReviewDecision;
use codex_protocol::protocol::TurnEnvironmentSelection;
use codex_utils_path_uri::PathUri;
use futures::future::BoxFuture;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct PreToolUseExecutionTarget {
    environment_id: String,
    cwd: PathUri,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) exec_command_shell: Option<ExecCommandShellTarget>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ExecCommandShellTarget {
    pub(crate) shell_type: ShellType,
    pub(crate) executable_path: PathBuf,
}

impl From<TurnEnvironmentSelection> for PreToolUseExecutionTarget {
    fn from(selection: TurnEnvironmentSelection) -> Self {
        Self {
            environment_id: selection.environment_id,
            cwd: selection.cwd,
            exec_command_shell: None,
        }
    }
}

pub(crate) struct PreToolUseApprovalReceipt {
    call_id: String,
    payload: ToolPayload,
    execution_target: Option<PreToolUseExecutionTarget>,
    reviewed_action_truncated: bool,
}

impl PreToolUseApprovalReceipt {
    pub(in crate::tools) fn for_reviewed(
        invocation: &ToolInvocation,
        execution_target: Option<PreToolUseExecutionTarget>,
        reviewed_action_truncated: bool,
    ) -> Self {
        Self {
            call_id: invocation.call_id.clone(),
            payload: invocation.payload.clone(),
            execution_target,
            reviewed_action_truncated,
        }
    }

    pub(crate) fn authorizes(
        &self,
        call_id: &str,
        payload: &ToolPayload,
        execution_target: Option<&PreToolUseExecutionTarget>,
    ) -> bool {
        !self.reviewed_action_truncated
            && self.call_id == call_id
            && &self.payload == payload
            && self.execution_target.as_ref() == execution_target
    }

    pub(crate) fn execution_target(&self) -> Option<&PreToolUseExecutionTarget> {
        self.execution_target.as_ref()
    }
}

pub(crate) fn review<'a>(
    invocation: &'a ToolInvocation,
    payload: &'a PreToolUsePayload,
    reason: String,
) -> BoxFuture<'a, Result<PreToolUseApprovalReceipt, String>> {
    Box::pin(async move {
        let request = GuardianApprovalRequest::PreToolUse {
            id: invocation.call_id.clone(),
            tool_name: payload.tool_name.name().to_string(),
            tool_input: payload.tool_input.clone(),
            execution_target: payload
                .execution_target
                .as_ref()
                .map(|target| serde_json::json!(target)),
            reason,
            #[allow(deprecated)]
            cwd: invocation.turn.cwd.clone(),
        };
        let action = preflight_guardian_action(&request).map_err(|_| {
            "The action could not be prepared for an exact automatic approval review. The tool call was blocked."
                .to_string()
        })?;
        if action.truncated {
            return Err(
                "The action was too large for an exact automatic approval review. Move long inline scripts into a file or shorten the command, then retry."
                    .to_string(),
            );
        }
        let review_id = new_guardian_review_id();
        let decision = review_pre_tool_use(
            Arc::clone(&invocation.session),
            (&invocation.step_context).into(),
            review_id,
            request,
        )
        .await;
        match decision {
            ReviewDecision::Approved
            | ReviewDecision::ApprovedForSession
            | ReviewDecision::ApprovedExecpolicyAmendment { .. }
            | ReviewDecision::ApprovedMcpPolicyAmendment
            | ReviewDecision::NetworkPolicyAmendment { .. } => {
                Ok(PreToolUseApprovalReceipt::for_reviewed(
                    invocation,
                    payload.execution_target.clone(),
                    /*reviewed_action_truncated*/ false,
                ))
            }
            ReviewDecision::Denied { rejection } => Err(rejection),
            ReviewDecision::TimedOut => Err(codex_prompts::ResolvedModelMessages::from_model(
                &invocation.step_context.settings.model_info,
            )
            .auto_review()
            .timeout_instructions
            .to_string()),
            ReviewDecision::Abort => Err(
                "Automatic approval review was cancelled. The tool call was blocked.".to_string(),
            ),
        }
    })
}
