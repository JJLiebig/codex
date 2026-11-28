use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::NetworkSandboxPolicy;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::ThreadSettingsAppliedEvent;
use codex_protocol::protocol::ThreadSettingsSnapshot;
use codex_protocol::protocol::WarningEvent;
use codex_utils_absolute_path::AbsolutePathBuf;

use super::*;

#[tokio::test]
async fn copied_history_can_have_an_owned_checkpoint_between_many_records() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("rollout-copied.jsonl");
    let mut file = std::fs::File::create(&path)?;
    let id = ThreadId::new();
    let parent = ThreadId::new();
    let mut append = |item| -> anyhow::Result<()> {
        let line = RolloutLine {
            timestamp: "2026-11-28T00:00:00Z".to_owned(),
            ordinal: None,
            item,
        };
        serde_json::to_writer(&mut file, &line)?;
        writeln!(file)?;
        Ok(())
    };
    append(RolloutItem::SessionMeta(Box::new(SessionMetaLine {
        meta: codex_rollout::SessionMeta {
            id,
            session_id: id.into(),
            forked_from_id: Some(parent),
            source: SessionSource::Cli,
            ..Default::default()
        },
        git: None,
    })))?;
    append(RolloutItem::EventMsg(EventMsg::Warning(WarningEvent {
        message: "f".repeat(300 * 1024),
    })))?;
    append(RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(
        ThreadSettingsAppliedEvent {
            thread_id: Some(id),
            thread_settings: ThreadSettingsSnapshot {
                model: "fixture".to_owned(),
                model_provider_id: "fixture".to_owned(),
                service_tier: None,
                approval_policy: AskForApproval::Never,
                approvals_reviewer: ApprovalsReviewer::User,
                requested_approvals_reviewer: None,
                permission_profile: PermissionProfile::read_only(),
                active_permission_profile: None,
                cwd: AbsolutePathBuf::current_dir()?,
                runtime_workspace_roots: None,
                reasoning_effort: None,
                reasoning_summary: None,
                personality: None,
                collaboration_mode: CollaborationMode {
                    mode: ModeKind::Default,
                    settings: Settings {
                        model: "fixture".to_owned(),
                        reasoning_effort: None,
                        developer_instructions: None,
                    },
                },
                disabled_plugin_ids: Vec::new(),
            },
        },
    )))?;
    for _ in 0..40 {
        append(RolloutItem::EventMsg(EventMsg::Warning(WarningEvent {
            message: "later record".to_owned(),
        })))?;
    }
    drop(file);
    assert!(saved_root_has_settings(&path, id).await);
    Ok(())
}

#[test]
fn unnamed_materialized_grants_cannot_arm_recovery() {
    let root = AbsolutePathBuf::current_dir().expect("test cwd");
    let profile = PermissionProfile::workspace_write_with(
        &[root],
        NetworkSandboxPolicy::Restricted,
        /*exclude_tmpdir_env_var*/ false,
        /*exclude_slash_tmp*/ false,
    );

    assert!(!permission_profile_is_resumable(
        &profile, /*active*/ None
    ));
    assert!(permission_profile_is_resumable(
        &profile,
        Some(&ActivePermissionProfile::new("work")),
    ));
    assert!(permission_profile_is_resumable(
        &PermissionProfile::read_only(),
        /*active*/ None,
    ));
}
