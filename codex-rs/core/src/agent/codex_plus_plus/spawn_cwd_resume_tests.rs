use super::*;
use crate::session::tests::make_session_and_context;
use codex_history::ResumedHistory;
use codex_protocol::protocol::ThreadSettingsAppliedEvent;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use std::sync::Arc;

#[test_case::test_case(true; "explicit cwd")]
#[test_case::test_case(false; "inherited cwd")]
#[tokio::test]
async fn restore_uses_child_owned_location_with_current_permissions(explicit: bool) {
    let (session, turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    let permissions = config.permissions.permission_profile().clone();
    let directory = tempfile::tempdir().expect("child directory");
    let expected = AbsolutePathBuf::try_from(directory.path()).expect("absolute path");
    let child = ThreadId::new();
    let mut settings = session.thread_settings_snapshot().await;
    settings.cwd = expected.clone();
    let child_settings = RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(
        ThreadSettingsAppliedEvent {
            thread_id: Some(child),
            thread_settings: settings,
        },
    ));
    let parent_settings = RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(
        ThreadSettingsAppliedEvent {
            thread_id: Some(session.thread_id),
            thread_settings: session.thread_settings_snapshot().await,
        },
    ));
    let meta = RolloutItem::SessionMeta(Box::new(codex_protocol::protocol::SessionMetaLine {
        meta: codex_protocol::protocol::SessionMeta {
            id: child,
            codex_plus_plus_spawn_cwd: explicit,
            ..Default::default()
        },
        git: None,
    }));
    let parent_meta =
        RolloutItem::SessionMeta(Box::new(codex_protocol::protocol::SessionMetaLine {
            meta: codex_protocol::protocol::SessionMeta {
                id: session.thread_id,
                codex_plus_plus_spawn_cwd: true,
                ..Default::default()
            },
            git: None,
        }));
    let history = InitialHistory::Resumed(ResumedHistory {
        conversation_id: child,
        history_revision: None,
        history: Arc::new(vec![meta, parent_meta, child_settings, parent_settings]),
        rollout_path: None,
    });
    let requests = restore(
        &mut config,
        Some(&turn.initial_environments),
        &history,
        child,
        turn.config.cwd.as_path(),
    )
    .expect("restore");
    if !explicit {
        assert!(requests.is_none());
        assert_eq!(config.cwd, turn.config.cwd);
        return;
    }
    let requests = requests.expect("child requests");
    assert_eq!(config.cwd, expected);
    assert_eq!(
        requests
            .iter()
            .find(|request| request.environment_id == codex_exec_server::LOCAL_ENVIRONMENT_ID)
            .expect("local")
            .cwd,
        PathUri::from_abs_path(&expected)
    );
    assert_eq!(config.permissions.permission_profile(), &permissions);
}
