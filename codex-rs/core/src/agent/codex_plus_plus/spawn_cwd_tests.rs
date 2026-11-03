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
async fn cwd_preserves_explicit_workspace_roots_and_permissions() {
    let (_session, turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    config.workspace_roots_explicit = true;
    config.workspace_roots_include_cwd = false;
    let explicit_root = tempfile::tempdir().expect("explicit root");
    config.workspace_roots = vec![
        config.cwd.clone(),
        AbsolutePathBuf::try_from(explicit_root.path()).expect("absolute root"),
    ];
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
    expected
        .iter_mut()
        .find(|request| request.environment_id == LOCAL_ENVIRONMENT_ID)
        .expect("local")
        .cwd = PathUri::from_abs_path(&config.cwd);
    assert_eq!(requests, expected);
    config.cwd = original.cwd.clone();
    config.subagent_cwd_override = false;
    assert_eq!(config, original);
}

#[tokio::test]
async fn cwd_rebases_parent_root_beside_additional_writable_roots() {
    let (_session, turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    let extra = tempfile::tempdir().expect("extra root");
    let extra = AbsolutePathBuf::try_from(extra.path()).expect("absolute root");
    config.workspace_roots_explicit = true;
    config.workspace_roots = vec![config.cwd.clone(), extra.clone()];
    let directory = tempfile::tempdir().expect("directory");
    apply(
        &mut config,
        &turn.initial_environments,
        Some(directory.path().to_str().expect("path")),
    )
    .expect("override");
    assert_eq!(
        config.workspace_roots,
        vec![
            AbsolutePathBuf::try_from(directory.path()).expect("absolute cwd"),
            extra
        ]
    );
    assert_eq!(config.permissions.workspace_roots(), config.workspace_roots);
}

#[tokio::test]
async fn cwd_rebases_roots_forwarded_by_native_startup() {
    let (_session, turn) = make_session_and_context().await;
    let extra = tempfile::tempdir().expect("extra root");
    let extra = AbsolutePathBuf::try_from(extra.path()).expect("absolute root");
    let mut config = crate::config::ConfigBuilder::without_managed_config_for_tests()
        .codex_home(turn.config.codex_home.to_path_buf())
        .harness_overrides(crate::config::ConfigOverrides {
            cwd: Some(turn.config.cwd.to_path_buf()),
            workspace_roots: Some(vec![turn.config.cwd.clone(), extra.clone()]),
            workspace_roots_include_cwd: Some(true),
            ..Default::default()
        })
        .build()
        .await
        .expect("forwarded native config");
    let directory = tempfile::tempdir().expect("child directory");
    apply(
        &mut config,
        &turn.initial_environments,
        Some(directory.path().to_str().expect("path")),
    )
    .expect("override");
    assert_eq!(
        config.workspace_roots,
        vec![
            AbsolutePathBuf::try_from(directory.path()).expect("absolute cwd"),
            extra
        ]
    );
}
