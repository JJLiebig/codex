use super::*;
use crate::session::tests::make_session_and_context;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn omitted_cwd_inherits_without_changing_config() {
    let (_session, turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    let original = config.clone();
    assert!(
        apply(&mut config, &turn.initial_environments, /*cwd*/ None)
            .expect("inherit")
            .is_none()
    );
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

#[tokio::test]
async fn workspace_sandbox_uses_new_cwd_without_changing_policy() {
    let (_session, turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    config
        .permissions
        .set_permission_profile(codex_protocol::models::PermissionProfile::workspace_write())
        .expect("workspace policy");
    let policy = config.permissions.permission_profile().clone();
    let directory = tempfile::tempdir().expect("child directory");
    apply(
        &mut config,
        &turn.initial_environments,
        Some(directory.path().to_str().expect("path")),
    )
    .expect("cwd");
    assert_eq!(config.permissions.permission_profile(), &policy);
    assert!(
        config
            .permissions
            .effective_permission_profile()
            .file_system_sandbox_policy()
            .can_write_local_path_with_cwd(directory.path(), config.cwd.as_path())
    );
}

#[tokio::test]
async fn cwd_updates_ready_environment_roots_without_changing_its_policy() {
    use crate::environment_selection::EnvironmentConfigOrigin;
    use crate::environment_selection::TurnEnvironmentState;
    let (_session, turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    let mut environments = turn.initial_environments.clone();
    let TurnEnvironmentState::Ready(environment) = &mut environments.environments[0] else {
        panic!("ready local environment");
    };
    environment.config_origin = EnvironmentConfigOrigin::Owner;
    let mut expected_owner = environment.config().clone();
    let directory = tempfile::tempdir().expect("child directory");
    let expected_cwd =
        PathUri::from_abs_path(&AbsolutePathBuf::try_from(directory.path()).expect("absolute cwd"));
    expected_owner.workspace_roots = vec![expected_cwd.clone()];
    let requests = apply(
        &mut config,
        &environments,
        Some(directory.path().to_str().expect("path")),
    )
    .expect("cwd override")
    .expect("requests");
    assert_eq!(requests[0].cwd, expected_cwd);
    assert_eq!(
        requests[0].config,
        EnvironmentConfigState::Ready(expected_owner)
    );
}

#[test_case::test_case("file:///remote/project", "../worktree", "file:///remote/worktree"; "posix executor")]
#[test_case::test_case("file:///C:/remote/project", "..\\worktree", "file:///C:/remote/worktree"; "windows executor")]
#[tokio::test]
async fn cwd_resolves_on_the_selected_executor(base: &str, requested: &str, expected: &str) {
    use crate::environment_selection::TurnEnvironmentState;
    let (_session, turn) = make_session_and_context().await;
    let mut config = (*turn.config).clone();
    let original = config.clone();
    let mut environments = turn.initial_environments.clone();
    let TurnEnvironmentState::Ready(environment) = &mut environments.environments[0] else {
        panic!("ready environment");
    };
    environment.selection.environment_id = "remote".to_string();
    environment.selection.cwd = PathUri::parse(base).expect("executor cwd");
    environment.selection.workspace_roots = vec![environment.selection.cwd.clone()];
    let requests = apply(&mut config, &environments, Some(requested))
        .expect("cwd override")
        .expect("requests");
    let expected = PathUri::parse(expected).expect("child cwd");
    assert_eq!(
        (&requests[0].cwd, &requests[0].workspace_roots),
        (&expected, &vec![expected.clone()])
    );
    assert_eq!(config, original);
}
