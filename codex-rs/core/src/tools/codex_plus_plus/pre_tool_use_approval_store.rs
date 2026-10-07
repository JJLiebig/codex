use std::cell::Cell;
use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use super::pre_tool_use_review::ExecCommandShellTarget;
use super::pre_tool_use_review::PreToolUseExecutionTarget;
use crate::shell::Shell;
use crate::shell::ShellType;
use crate::tools::handlers::unified_exec::ExecCommandArgs;
use crate::tools::handlers::unified_exec::get_command;
use crate::tools::hook_names::HookToolName;
use codex_exec_server::Environment;
use codex_tools::UnifiedExecShellMode;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExactPreToolUseApproval {
    execution_target: Option<PreToolUseExecutionTarget>,
}

impl ExactPreToolUseApproval {
    pub(crate) fn new(execution_target: Option<PreToolUseExecutionTarget>) -> Self {
        Self { execution_target }
    }

    fn matches_local_exec_command_shell(&self, resolved_shell: &ExecCommandShellTarget) -> bool {
        resolved_shell.executable_path.is_absolute()
            && self
                .execution_target
                .as_ref()
                .and_then(|target| target.exec_command_shell.as_ref())
                == Some(resolved_shell)
    }
}

pub(crate) struct ResolvedExecCommandShell {
    pub(crate) hook_tool_name: HookToolName,
    pub(crate) execution_target: Option<ExecCommandShellTarget>,
}

fn exec_command_hook_tool_name(shell_type: ShellType) -> HookToolName {
    match shell_type {
        ShellType::Bash | ShellType::Sh | ShellType::Zsh => HookToolName::bash(),
        ShellType::PowerShell => HookToolName::new("PowerShell").with_matcher_alias("Bash"),
        ShellType::Cmd => HookToolName::new("cmd.exe").with_matcher_alias("Bash"),
    }
}

pub(crate) fn resolved_exec_command_shell(
    environment: &Environment,
    resolve_local: impl FnOnce() -> Option<(ShellType, String)>,
) -> Option<ResolvedExecCommandShell> {
    if environment.is_remote() {
        return Some(ResolvedExecCommandShell {
            hook_tool_name: HookToolName::bash(),
            execution_target: None,
        });
    }
    let (shell_type, executable_path) = resolve_local()?;
    Some(ResolvedExecCommandShell {
        hook_tool_name: exec_command_hook_tool_name(shell_type),
        execution_target: Some(ExecCommandShellTarget {
            shell_type,
            executable_path: PathBuf::from(executable_path),
        }),
    })
}

pub(crate) fn reviewed_exec_command_shell(
    environment: &Environment,
    args: &ExecCommandArgs,
    shell: Arc<Shell>,
    shell_mode: &UnifiedExecShellMode,
    allow_login_shell: bool,
) -> Option<ResolvedExecCommandShell> {
    resolved_exec_command_shell(environment, || {
        let resolved_command = get_command(args, shell, shell_mode, allow_login_shell).ok()?;
        Some((
            resolved_command.shell.shell.shell_type,
            resolved_command.command.first()?.clone(),
        ))
    })
}

pub(crate) fn exec_command_shell_matches_review(
    reviewed_shell: Option<&ExecCommandShellTarget>,
    resolved_shell: Option<&ExecCommandShellTarget>,
) -> bool {
    reviewed_shell.is_none_or(|reviewed_shell| Some(reviewed_shell) == resolved_shell)
}

pub(crate) struct ExecCommandApprovalContext<'a> {
    pub(crate) environment: &'a Environment,
    pub(crate) requested_shell: Option<&'a str>,
    pub(crate) use_login_shell: bool,
    pub(crate) tty: bool,
    pub(crate) resolved_shell: Option<&'a ExecCommandShellTarget>,
}

pub(crate) fn exact_exec_command_approval(
    approval: Option<&ExactPreToolUseApproval>,
    context: ExecCommandApprovalContext<'_>,
) -> bool {
    let Some(approval) = approval else {
        return false;
    };
    if context.use_login_shell
        || context.tty
        || context
            .requested_shell
            .is_some_and(|shell| context.environment.is_remote() || !Path::new(shell).is_absolute())
    {
        return false;
    }
    if context.environment.is_remote() {
        approval
            .execution_target
            .as_ref()
            .is_some_and(|target| target.exec_command_shell.is_none())
    } else {
        context
            .resolved_shell
            .is_some_and(|shell| approval.matches_local_exec_command_shell(shell))
    }
}

tokio::task_local! {
    static EXACT_APPROVAL: Cell<Option<ExactPreToolUseApproval>>;
    static REVIEWED_EXEC_COMMAND_SHELL: Cell<Option<ExecCommandShellTarget>>;
}

pub(crate) async fn scope<T>(
    approval: Option<ExactPreToolUseApproval>,
    reviewed_exec_command_shell: Option<ExecCommandShellTarget>,
    future: impl Future<Output = T>,
) -> T {
    REVIEWED_EXEC_COMMAND_SHELL
        .scope(
            Cell::new(reviewed_exec_command_shell),
            EXACT_APPROVAL.scope(Cell::new(approval), future),
        )
        .await
}

pub(crate) fn take() -> Option<ExactPreToolUseApproval> {
    EXACT_APPROVAL.try_with(Cell::take).unwrap_or(None)
}

pub(crate) fn take_reviewed_exec_command_shell() -> Option<ExecCommandShellTarget> {
    REVIEWED_EXEC_COMMAND_SHELL
        .try_with(Cell::take)
        .unwrap_or(None)
}

#[cfg(test)]
#[path = "pre_tool_use_approval_store_tests.rs"]
mod tests;
