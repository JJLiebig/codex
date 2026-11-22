//! Ties the sandbox's PID namespace to the bubblewrap monitor that launched it.
//!
//! Bubblewrap's `--die-with-parent` arms the parent-death signal just before it execs the inner
//! stage, but the kernel clears that setting on a secure exec. AppArmor's profile for system
//! bubblewrap on Ubuntu (`bwrap//&unpriv_bwrap`) makes that exec secure. The inner stage is PID 1
//! of the sandbox and leads its own session, so neither owner shutdown nor owner death reaches it
//! without that signal, and the whole namespace outlives its launcher.
//!
//! The inner stage therefore arms the signal itself. PID 1 cannot see its parent, so `getppid()`
//! cannot reveal a monitor that died first; the outer stage passes a pidfd for it instead.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::fd::RawFd;

/// Hidden inner-stage option that carries the monitor pidfd.
pub(crate) const MONITOR_PIDFD_OPTION: &str = "monitor-pidfd";

/// Run in the process that is about to exec bubblewrap, which stays as the monitor that parents
/// the sandbox's PID 1: hand the inner stage a pidfd for this process.
pub(crate) fn pass_monitor_pidfd(argv: &mut Vec<String>, preserved_files: &mut Vec<File>) {
    // Without pidfd_open (Linux before 5.3) the inner stage still arms its signal; only the
    // check for a monitor that exited during startup is lost.
    let Ok(pidfd) = rustix::process::pidfd_open(
        rustix::process::getpid(),
        rustix::process::PidfdFlags::empty(),
    ) else {
        return;
    };
    let inner_program = argv
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or_else(|| panic!("bubblewrap argv is missing command separator '--'"))
        + 1;
    argv.splice(
        inner_program + 1..inner_program + 1,
        [
            format!("--{MONITOR_PIDFD_OPTION}"),
            pidfd.as_raw_fd().to_string(),
        ],
    );
    preserved_files.push(File::from(pidfd));
}

/// Run first in the inner stage: die with the bubblewrap monitor, and exit before starting the
/// command if the monitor is already gone.
pub(crate) fn bind_to_monitor(monitor_pidfd: Option<RawFd>) {
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } < 0 {
        let err = std::io::Error::last_os_error();
        panic!("failed to set sandbox parent-death signal: {err}");
    }
    let Some(fd) = monitor_pidfd else {
        return;
    };
    // SAFETY: the outer stage passes this descriptor to the inner stage alone. Dropping it before
    // the command starts keeps it out of the sandboxed command.
    let pidfd = unsafe { OwnedFd::from_raw_fd(fd) };
    // exit_notify() reparents the monitor's children, sending their parent-death signal, and
    // publishes its exit to pidfd pollers within one tasklist_lock section. kill(0, 0) takes that
    // lock, so afterwards either the monitor saw the armed signal or the poll sees its exit.
    unsafe {
        libc::kill(/*pid*/ 0, /*sig*/ 0)
    };
    let mut monitor = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe {
        libc::poll(&mut monitor, /*nfds*/ 1, /*timeout*/ 0)
    };
    if ready < 0 || monitor.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
        let err = std::io::Error::last_os_error();
        panic!("failed to check the bubblewrap monitor: {err}");
    }
    if ready > 0 {
        // The monitor exited before the signal was armed. A namespace init ignores SIGKILL from
        // itself, so exit; the kernel then ends anything else in the namespace.
        std::process::exit(1);
    }
}
