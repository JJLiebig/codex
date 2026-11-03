use super::*;
use crate::session::tests::make_session_and_context;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn omitted_or_invalid_cwd_leaves_config_unchanged() {
    let (_session, turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    let original = config.clone();
    assert!(
        apply(&mut config, &turn.initial_environments, None)
            .expect("inherit")
            .is_none()
    );
    let missing = tempfile::tempdir().expect("temporary directory");
    let missing = missing.path().join("missing");
    assert!(
        apply(
            &mut config,
            &turn.initial_environments,
            Some(missing.to_str().expect("path"))
        )
        .is_err()
    );
    assert!(apply(&mut config, &turn.initial_environments, Some("")).is_err());
    assert_eq!(config, original);
}

#[tokio::test]
async fn cwd_preserves_additional_roots_and_permissions() {
    let (_session, turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    let extra = tempfile::tempdir().expect("additional root");
    config.workspace_roots = vec![AbsolutePathBuf::try_from(extra.path()).expect("absolute extra")];
    config
        .permissions
        .set_workspace_roots(config.workspace_roots.clone());
    let original = config.clone();
    let directory = tempfile::tempdir().expect("directory");
    let requests = apply(
        &mut config,
        &turn.initial_environments,
        Some(directory.path().to_str().expect("path")),
    )
    .expect("cwd override")
    .expect("explicit requests");
    let mut expected = turn.initial_environments.inheritable_requests();
    let local = expected
        .iter_mut()
        .find(|request| request.environment_id == LOCAL_ENVIRONMENT_ID)
        .expect("local");
    local.cwd = PathUri::from_abs_path(&config.cwd);
    local.workspace_roots = vec![local.cwd.clone()];
    assert_eq!(requests, expected);
    config.cwd = original.cwd.clone();
    assert_eq!(config, original);
}
