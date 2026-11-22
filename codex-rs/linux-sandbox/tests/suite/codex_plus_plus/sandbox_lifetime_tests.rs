//! A sandboxed command's whole process tree must end with its launcher, including a quiet
//! descendant in its own session that never writes another byte.

use super::*;
use pretty_assertions::assert_eq;
use rustix::process::Pid;
use rustix::process::PidfdFlags;
use rustix::process::Signal;
use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::BufReader;
use tokio::time::timeout;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    /// Owner shutdown kills the launcher's process group, as session termination does.
    KillGroup,
    /// The owner exits: the thread that spawned the launcher ends, firing its parent-death signal.
    OwnerExit,
}

/// Linux before 5.3 and some seccomp profiles lack pidfd_open. The launcher still works there,
/// but these tests need pidfds to watch and clean up what they start.
fn pidfds_unavailable() -> bool {
    rustix::process::pidfd_open(rustix::process::getpid(), PidfdFlags::empty()).is_err()
}

#[tokio::test]
async fn sandboxed_tree_ends_with_its_launcher() -> io::Result<()> {
    if pidfds_unavailable() {
        eprintln!("skipping sandbox lifetime test: pidfds are unavailable");
        return Ok(());
    }
    if should_skip_bwrap_tests().await {
        eprintln!("skipping bwrap test: bubblewrap is unavailable");
        return Ok(());
    }
    let cwd = tempfile::tempdir()?;
    let mut survived = Vec::new();
    for (name, profile) in [
        ("read-only", PermissionProfile::read_only()),
        ("workspace-write", PermissionProfile::workspace_write()),
    ] {
        for ending in [Ending::KillGroup, Ending::OwnerExit] {
            if !tree_ends_with_launcher(cwd.path(), &profile, ending).await? {
                survived.push((name, ending));
            }
        }
    }
    assert_eq!(
        survived,
        Vec::new(),
        "sandboxed processes outlived their launcher"
    );
    Ok(())
}

async fn tree_ends_with_launcher(
    cwd: &Path,
    profile: &PermissionProfile,
    ending: Ending,
) -> io::Result<bool> {
    let mut env = create_env_from_core_vars();
    strip_proxy_env(&mut env);
    let mut command = linux_sandbox_launcher(cwd, profile, /*managed_network*/ None, env);
    command
        .args([
            "--",
            "bash",
            "-c",
            "setsid sleep infinity & echo ready; wait",
        ])
        .kill_on_drop(true);
    // Launch as Codex does: a new session, terminated when the spawning thread ends.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let runtime = tokio::runtime::Handle::current();
    let (spawned_tx, spawned) = tokio::sync::oneshot::channel();
    let (owner_exit, exit_requested) = std::sync::mpsc::channel::<()>();
    let owner = std::thread::spawn(move || {
        let _runtime = runtime.enter();
        let _ = spawned_tx.send(command.spawn());
        let _ = exit_requested.recv();
    });
    let mut owner_exit = Some(owner_exit);
    let mut launcher = spawned.await.expect("owner thread reports the spawn")?;

    let mut stdout = BufReader::new(launcher.stdout.take().unwrap());
    let mut ready = String::new();
    timeout(SANDBOX_TIMEOUT, stdout.read_line(&mut ready)).await??;
    if ready != "ready\n" {
        let mut stderr = String::new();
        launcher
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .await?;
        panic!("sandboxed command did not start: {stderr}");
    }
    let launcher_pid = launcher.id().unwrap().try_into().unwrap();
    let tree = process_tree(launcher_pid)?;

    match ending {
        Ending::KillGroup => assert_eq!(unsafe { libc::killpg(launcher_pid, libc::SIGKILL) }, 0),
        Ending::OwnerExit => drop(owner_exit.take()),
    }
    // Stdout reaches EOF only once every process holding it has exited.
    let ended = timeout(OPERATION_TIMEOUT, stdout.read_to_end(&mut Vec::new()))
        .await
        .is_ok();
    if !ended {
        for process in &tree {
            let _ = rustix::process::pidfd_send_signal(process, Signal::KILL);
        }
    }
    drop(owner_exit);
    owner.join().expect("owner thread exits");
    launcher.wait().await?;
    Ok(ended)
}

/// Pidfds for the launcher and its descendants, so a failing run kills exactly what it started.
fn process_tree(launcher: i32) -> io::Result<Vec<OwnedFd>> {
    let mut tree = Vec::new();
    let mut pending = vec![launcher];
    while let Some(pid) = pending.pop() {
        tree.push(rustix::process::pidfd_open(
            Pid::from_raw(pid).unwrap(),
            PidfdFlags::empty(),
        )?);
        for task in fs::read_dir(format!("/proc/{pid}/task"))? {
            let children = fs::read_to_string(task?.path().join("children"))?;
            pending.extend(
                children
                    .split_whitespace()
                    .map(|child| child.parse::<i32>().unwrap()),
            );
        }
    }
    Ok(tree)
}

#[tokio::test]
async fn inner_stage_does_not_start_after_its_monitor_exits() -> io::Result<()> {
    if pidfds_unavailable() {
        eprintln!("skipping monitor check test: pidfds are unavailable");
        return Ok(());
    }
    let mut monitor = std::process::Command::new("true").spawn()?;
    let monitor_pidfd = rustix::process::pidfd_open(
        Pid::from_raw(monitor.id().try_into().unwrap()).unwrap(),
        PidfdFlags::empty(),
    )?;
    monitor.wait()?;
    let fd = monitor_pidfd.as_raw_fd();
    let cwd = tempfile::tempdir()?;
    let mut command = linux_sandbox_launcher(
        cwd.path(),
        &PermissionProfile::read_only(),
        /*managed_network*/ None,
        create_env_from_core_vars(),
    );
    command.args([
        "--apply-seccomp-then-exec",
        "--monitor-pidfd",
        &fd.to_string(),
        "--",
        "bash",
        "-c",
        "echo started",
    ]);
    // Bubblewrap passes the monitor pidfd through to the inner stage.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = command.output().await?;
    assert_eq!(
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stdout)
        ),
        (Some(1), "".into()),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
