use super::LaunchDesktop;
use crate::winutil::to_wide;

impl LaunchDesktop {
    /// Keeps unrestricted current-user commands on the interactive desktop.
    pub fn current_user() -> Self {
        Self {
            _private_desktop: None,
            startup_name: to_wide("Winsta0\\Default"),
        }
    }
}
