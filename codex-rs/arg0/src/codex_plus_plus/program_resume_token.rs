//! Consume the pane's credential before helper dispatch, dotenv loading or runtime startup.

use std::sync::OnceLock;

pub(crate) const ENV_VAR: &str = "OSC7577_TOKEN";
static TOKEN: OnceLock<Option<String>> = OnceLock::new();

/// Consume the pane credential before any startup code changes the environment.
///
/// # Safety
/// Call only while startup is still single-threaded.
pub unsafe fn capture() {
    TOKEN.get_or_init(|| {
        let token = std::env::var_os(ENV_VAR)?;
        // Linux retains the initial environment in procfs after unsetenv. Some restricted
        // tools share the host PID namespace, so protect this process before spawning any.
        #[cfg(target_os = "linux")]
        if codex_process_hardening::disable_process_dumping().is_err() {
            use std::os::unix::process::CommandExt;

            // Execute the same image with a clean initial environment. Keep argv[0] for
            // helper dispatch and preserve the rest of the invocation.
            let mut args = std::env::args_os();
            let argv0 = args.next().unwrap_or_default();
            let error = std::process::Command::new("/proc/self/exe")
                .arg0(argv0)
                .args(args)
                .env_remove(ENV_VAR)
                .exec();
            eprintln!("Could not restart Codex with a clean environment: {error}");
            std::process::exit(1);
        }
        // SAFETY: the caller runs on the startup thread before any other threads exist.
        unsafe { std::env::remove_var(ENV_VAR) };
        token.into_string().ok().filter(|token| {
            (22..=64).contains(&token.len())
                && token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        })
    });
}

pub fn get() -> Option<&'static str> {
    TOKEN.get()?.as_deref()
}
