use codex_protocol::protocol::NetworkSandboxPolicy;
use codex_utils_absolute_path::AbsolutePathBuf;

use super::*;

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
