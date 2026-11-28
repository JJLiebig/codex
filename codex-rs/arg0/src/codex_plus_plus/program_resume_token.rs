//! Consume the pane's credential before helper dispatch, dotenv loading or runtime startup.

use std::sync::OnceLock;

pub(crate) const ENV_VAR: &str = "OSC7577_TOKEN";
static TOKEN: OnceLock<Option<String>> = OnceLock::new();

pub(super) fn capture() {
    TOKEN.get_or_init(|| {
        let token = std::env::var(ENV_VAR).ok();
        // SAFETY: arg0 dispatch runs on the startup thread before any other threads exist.
        unsafe { std::env::remove_var(ENV_VAR) };
        // Linux retains the initial environment in procfs after unsetenv. Some restricted
        // tools share the host PID namespace, so protect this process before spawning any.
        #[cfg(target_os = "linux")]
        if token.is_some() && codex_process_hardening::disable_process_dumping().is_err() {
            return None;
        }
        token.filter(|token| {
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
